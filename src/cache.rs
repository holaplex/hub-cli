use std::{
    borrow::Cow,
    fmt,
    io::{self, prelude::*},
    path::{Path, PathBuf},
    pin::pin,
    sync::Arc,
};

use anyhow::{Context, Result};
use log::{debug, trace};
use prost::Message;
use rocksdb::{BlockBasedOptions, ColumnFamilyRef, Options, ReadOptions, WriteOptions, DB};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt},
    task::spawn_blocking,
};
use xxhash_rust::xxh3::Xxh3;

use crate::cli::CacheOpts;

mod proto {
    include!(concat!(env!("OUT_DIR"), "/cache.asset_uploads.rs"));
    include!(concat!(env!("OUT_DIR"), "/cache.drop_mints.rs"));
    include!(concat!(env!("OUT_DIR"), "/cache.mint_random_queued.rs"));
}

pub use proto::{AssetUpload, CreationStatus, DropMint, MintRandomQueued as ProtoMintRandomQueued};

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Checksum(u128);

impl fmt::Debug for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{:032x}", self.0) }
}

impl Checksum {
    const BUF_SIZE: usize = 8 << 10;

    pub fn hash<H: std::hash::Hash>(val: &H) -> Self {
        let mut hasher = Xxh3::new();
        val.hash(&mut hasher);
        Self(hasher.digest128())
    }

    pub fn read_rewind<R: Read + Seek>(name: impl fmt::Debug, mut stream: R) -> Result<Self> {
        let pos = stream
            .stream_position()
            .with_context(|| format!("Error getting stream position of {name:?}"))?;
        let res = Self::read(&name, &mut stream);
        stream
            .seek(io::SeekFrom::Start(pos))
            .with_context(|| format!("Error rewinding stream for {name:?}"))?;

        res
    }

    pub fn read<R: Read>(name: impl fmt::Debug, mut stream: R) -> Result<Self> {
        let mut hasher = Xxh3::new();
        let mut buf = vec![0; Self::BUF_SIZE].into_boxed_slice();

        loop {
            match stream.read(&mut buf) {
                Ok(0) => break Ok(Self(hasher.digest128())),
                Ok(n) => hasher.update(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context(format!("Error checksumming {name:?}")),
            }
        }
    }

    pub async fn read_rewind_async<R: AsyncRead + AsyncSeek>(
        name: impl fmt::Debug,
        mut stream: R,
    ) -> Result<Self> {
        let mut stream = pin!(stream);
        let pos = stream
            .stream_position()
            .await
            .with_context(|| format!("Error getting stream position of {name:?}"))?;
        let res = Self::read_async(&name, &mut stream).await;
        stream
            .seek(io::SeekFrom::Start(pos))
            .await
            .with_context(|| format!("Error rewinding stream for {name:?}"))?;

        res
    }

    pub async fn read_async<R: AsyncRead>(name: impl fmt::Debug, stream: R) -> Result<Self> {
        let mut stream = pin!(stream);
        let mut hasher = Xxh3::new();
        let mut buf = vec![0; Self::BUF_SIZE].into_boxed_slice();

        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break Ok(Self(hasher.digest128())),
                Ok(n) => hasher.update(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context(format!("Error checksumming {name:?}")),
            }
        }
    }

    pub fn to_bytes(self) -> [u8; 16] { self.0.to_le_bytes() }
}

const DIR_NAME: &str = ".hub-cache";

fn cache_dir(path: impl AsRef<Path>) -> PathBuf { path.as_ref().join(DIR_NAME) }

struct CacheConfigInner {
    db_opts: Options,
    read_opts: ReadOptions,
    write_opts: WriteOptions,
}

#[derive(Clone)]
#[repr(transparent)]
pub struct CacheConfig(Arc<CacheConfigInner>);

impl CacheConfig {
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(opts: CacheOpts) -> Self {
        let CacheOpts {
            cache_threads,
            cache_write_buf,
            cache_lru_size,
        } = opts;

        let block_opts = {
            let mut o = BlockBasedOptions::default();

            o.set_bloom_filter(10.0, false);
            o.set_block_size(16 << 10);
            o.set_cache_index_and_filter_blocks(true);
            // o.set_optimize_filters_for_memory(true); // TODO: not implemented?
            o.set_pin_l0_filter_and_index_blocks_in_cache(true);

            let c = rocksdb::Cache::new_lru_cache(cache_lru_size);
            o.set_block_cache(&c);

            o
        };

        let db_opts = {
            let mut o = Options::default();

            o.create_if_missing(true);
            o.increase_parallelism(cache_threads.try_into().unwrap_or(i32::MAX));
            o.set_bytes_per_sync(1 << 20);
            o.set_keep_log_file_num(1);
            o.set_level_compaction_dynamic_level_bytes(true);
            o.set_max_background_jobs(6);
            o.set_write_buffer_size(cache_write_buf);
            o.set_max_total_wal_size(10 << 20);

            o.set_compression_type(rocksdb::DBCompressionType::Lz4);
            o.set_bottommost_compression_type(rocksdb::DBCompressionType::Zstd);
            o.set_bottommost_zstd_max_train_bytes(10 << 20, true);

            o.set_block_based_table_factory(&block_opts);

            o
        };

        let read_opts = {
            let mut o = ReadOptions::default();

            o.set_async_io(true);
            o.set_readahead_size(4 << 20);

            o
        };

        let write_opts = WriteOptions::default();

        Self(
            CacheConfigInner {
                db_opts,
                read_opts,
                write_opts,
            }
            .into(),
        )
    }

    fn db_opts(&self) -> &Options { &self.0.db_opts }

    fn read_opts(&self) -> &ReadOptions { &self.0.read_opts }

    fn write_opts(&self) -> &WriteOptions { &self.0.write_opts }
}

mod private {
    #[allow(clippy::wildcard_imports)]
    use super::*;

    pub trait CacheFamily<'a> {
        type Static: CacheFamily<'static>;

        const CF_NAME: &'static str;

        fn new(cache: Cow<'a, Cache>) -> Self;

        fn cache(&self) -> &Cow<'a, Cache>;

        fn to_static(&self) -> Self::Static {
            Self::Static::new(Cow::Owned(self.cache().as_ref().to_owned()))
        }

        #[inline]
        fn get_cf(&'a self) -> Result<ColumnFamilyRef> { self.cache().get_cf(Self::CF_NAME) }
    }
}

use private::CacheFamily;

#[derive(Clone)]
pub struct Cache {
    dir: PathBuf,
    config: CacheConfig,
    db: Arc<DB>,
}

// NB: Don't use the async versions unless you absolutely have to, they're
//     going to all be slower.

impl Cache {
    pub async fn load(
        path: impl AsRef<Path> + Send + 'static,
        config: CacheConfig,
    ) -> Result<Self> {
        spawn_blocking(|| Self::load_sync(path, config))
            .await
            .unwrap()
    }

    pub fn load_sync(path: impl AsRef<Path>, config: CacheConfig) -> Result<Self> {
        debug!("Loading cache for {:?}", path.as_ref());

        let cache_dir = cache_dir(&path);
        let cfs = DB::list_cf(config.db_opts(), &cache_dir)
            .map_err(|e| debug!("Skipping column family load due to error: {e}"))
            .unwrap_or_default();

        trace!("Loading column handle(s) {cfs:?} for {:?}", path.as_ref());

        let db = DB::open_cf(config.db_opts(), cache_dir, cfs)
            .with_context(|| format!("Error opening cache database for {:?}", path.as_ref()))?;

        Ok(Self {
            dir: path.as_ref().into(),
            config,
            db: db.into(),
        })
    }

    fn create_cf(&self, name: &str) -> Result<()> {
        if self.db.cf_handle(name).is_some() {
            trace!("Reusing existing column handle {name:?} for {:?}", self.dir);

            return Ok(());
        }

        self.db
            .create_cf(name, self.config.db_opts())
            .with_context(|| {
                format!(
                    "Error creating column family {name:?} in cache database for {:?}",
                    self.dir
                )
            })
    }

    fn get_cf(&self, name: &str) -> Result<ColumnFamilyRef> {
        self.db.cf_handle(name).with_context(|| {
            format!(
                "No handle to column family {name:?} exists in cache database for {:?}",
                self.dir
            )
        })
    }

    pub async fn get<C: CacheFamily<'static> + Send + 'static>(&self) -> Result<C> {
        let this = self.clone();
        spawn_blocking(move || {
            this.create_cf(C::CF_NAME)?;

            Ok(C::new(Cow::Owned(this)))
        })
        .await
        .unwrap()
    }

    pub fn get_sync<'a, C: CacheFamily<'a>>(&'a self) -> Result<C> {
        self.create_cf(C::CF_NAME)?;

        Ok(C::new(Cow::Borrowed(self)))
    }
}

#[repr(transparent)]
pub struct AssetUploadCache<'a>(Cow<'a, Cache>);

impl<'a> CacheFamily<'a> for AssetUploadCache<'a> {
    type Static = AssetUploadCache<'static>;

    const CF_NAME: &'static str = "asset-uploads";

    #[inline]
    fn new(cache: Cow<'a, Cache>) -> Self { Self(cache) }

    #[inline]
    fn cache(&self) -> &Cow<'a, Cache> { &self.0 }
}

impl<'a> AssetUploadCache<'a> {
    pub async fn get(
        &self,
        path: impl AsRef<Path> + Send + 'static,
        ck: Checksum,
    ) -> Result<Option<AssetUpload>> {
        let this = self.to_static();
        spawn_blocking(move || this.get_sync(path, ck))
            .await
            .unwrap()
    }

    pub fn get_sync(&self, path: impl AsRef<Path>, ck: Checksum) -> Result<Option<AssetUpload>> {
        let path = path.as_ref();

        let bytes = self
            .0
            .db
            .get_cf_opt(
                &self.0.get_cf(Self::CF_NAME)?,
                ck.to_bytes(),
                self.0.config.read_opts(),
            )
            .with_context(|| format!("Error getting asset upload cache for {path:?}"))?;

        let Some(bytes) = bytes else { return Ok(None) };
        let upload = AssetUpload::decode(&*bytes).with_context(|| {
            format!("Error parsing asset upload cache for {path:?} (this should not happen)")
        })?;

        Ok(Some(upload))
    }

    pub async fn set(
        &self,
        path: impl AsRef<Path> + Send + 'static,
        ck: Checksum,
        upload: AssetUpload,
    ) -> Result<()> {
        let this = self.to_static();
        spawn_blocking(move || this.set_sync(path, ck, &upload))
            .await
            .unwrap()
    }

    pub fn set_sync(
        &self,
        path: impl AsRef<Path>,
        ck: Checksum,
        upload: &AssetUpload,
    ) -> Result<()> {
        let path = path.as_ref();
        let bytes = upload.encode_to_vec();

        self.0
            .db
            .put_cf_opt(
                &self.0.get_cf(Self::CF_NAME)?,
                ck.to_bytes(),
                bytes,
                self.0.config.write_opts(),
            )
            .with_context(|| format!("Error setting asset upload cache for {path:?}"))
    }
}

#[repr(transparent)]
pub struct AirdropWalletsCache<'a>(Cow<'a, Cache>);

impl<'a> CacheFamily<'a> for AirdropWalletsCache<'a> {
    type Static = AirdropWalletsCache<'static>;

    const CF_NAME: &'static str = "airdrop-wallets";

    #[inline]
    fn new(cache: Cow<'a, Cache>) -> Self { Self(cache) }

    #[inline]
    fn cache(&self) -> &Cow<'a, Cache> { &self.0 }
}

impl<'a> AirdropWalletsCache<'a> {
    pub async fn get(
        &self,
        path: impl AsRef<Path> + Send + 'static,
        pubkey: String,
    ) -> Result<Option<ProtoMintRandomQueued>> {
        let this = self.to_static();
        spawn_blocking(move || this.get_sync(path, pubkey))
            .await
            .unwrap()
    }

    pub fn get_sync(
        &self,
        path: impl AsRef<Path>,
        key: String,
    ) -> Result<Option<ProtoMintRandomQueued>> {
        let path = path.as_ref();

        let bytes = self
            .0
            .db
            .get_cf_opt(
                &self.0.get_cf(Self::CF_NAME)?,
                key.into_bytes(),
                self.0.config.read_opts(),
            )
            .with_context(|| format!("Error getting airdrop wallet for {path:?}"))?;

        let Some(bytes) = bytes else { return Ok(None) };
        let airdrop = ProtoMintRandomQueued::decode(&*bytes)
            .with_context(|| format!("Error parsing  for {path:?} (this should not happen)"))?;

        Ok(Some(airdrop))
    }

    pub async fn set(
        &self,
        path: impl AsRef<Path> + Send + 'static,
        key: &'static str,
        airdrop: ProtoMintRandomQueued,
    ) -> Result<()> {
        let this = self.to_static();
        spawn_blocking(move || this.set_sync(path, key, &airdrop))
            .await
            .unwrap()
    }

    pub fn set_sync(
        &self,
        path: impl AsRef<Path>,
        key: &str,
        airdrop: &ProtoMintRandomQueued,
    ) -> Result<()> {
        let path = path.as_ref();
        let bytes = airdrop.encode_to_vec();

        self.0
            .db
            .put_cf_opt(
                &self.0.get_cf(Self::CF_NAME)?,
                key.as_bytes(),
                bytes,
                self.0.config.write_opts(),
            )
            .with_context(|| format!("Error setting for {path:?}"))
    }
}

#[repr(transparent)]
pub struct DropMintCache<'a>(Cow<'a, Cache>);

impl<'a> CacheFamily<'a> for DropMintCache<'a> {
    type Static = DropMintCache<'static>;

    const CF_NAME: &'static str = "drop-mints";

    #[inline]
    fn new(cache: Cow<'a, Cache>) -> Self { Self(cache) }

    #[inline]
    fn cache(&self) -> &Cow<'a, Cache> { &self.0 }
}

impl<'a> DropMintCache<'a> {
    pub async fn get(
        &self,
        path: impl AsRef<Path> + Send + 'static,
        ck: Checksum,
    ) -> Result<Option<DropMint>> {
        let this = self.to_static();
        spawn_blocking(move || this.get_sync(path, ck))
            .await
            .unwrap()
    }

    pub fn get_sync(&self, path: impl AsRef<Path>, ck: Checksum) -> Result<Option<DropMint>> {
        let path = path.as_ref();

        let bytes = self
            .0
            .db
            .get_cf_opt(
                &self.0.get_cf(Self::CF_NAME)?,
                ck.to_bytes(),
                self.0.config.read_opts(),
            )
            .with_context(|| format!("Error getting asset upload cache for {path:?}"))?;

        let Some(bytes) = bytes else { return Ok(None) };
        let upload = DropMint::decode(&*bytes).with_context(|| {
            format!("Error parsing asset upload cache for {path:?} (this should not happen)")
        })?;

        Ok(Some(upload))
    }

    pub async fn set(
        &self,
        path: impl AsRef<Path> + Send + 'static,
        ck: Checksum,
        upload: DropMint,
    ) -> Result<()> {
        let this = self.to_static();
        spawn_blocking(move || this.set_sync(path, ck, &upload))
            .await
            .unwrap()
    }

    pub fn set_sync(&self, path: impl AsRef<Path>, ck: Checksum, upload: &DropMint) -> Result<()> {
        let path = path.as_ref();
        let bytes = upload.encode_to_vec();

        self.0
            .db
            .put_cf_opt(
                &self.0.get_cf(Self::CF_NAME)?,
                ck.to_bytes(),
                bytes,
                self.0.config.write_opts(),
            )
            .with_context(|| format!("Error setting asset upload cache for {path:?}"))
    }
}
