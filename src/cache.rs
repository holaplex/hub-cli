use std::{
    borrow::{Borrow, Cow},
    fmt,
    io::{self, prelude::*},
    marker::PhantomData,
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

use crate::{cli::CacheOpts, common::pubkey::Pubkey};

mod proto {
    #![allow(clippy::doc_markdown, clippy::trivially_copy_pass_by_ref)]

    include!(concat!(env!("OUT_DIR"), "/cache.asset_uploads.rs"));
    include!(concat!(env!("OUT_DIR"), "/cache.drop_mints.rs"));
    include!(concat!(env!("OUT_DIR"), "/cache.mint_random_queued.rs"));
}

pub use proto::{AssetUpload, CreationStatus, DropMint, MintRandomQueued};

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

    pub trait CacheFamily: Send + 'static {
        type KeyName: ?Sized + fmt::Debug + ToOwned<Owned = Self::OwnedKeyName>;
        type OwnedKeyName: fmt::Debug + Borrow<Self::KeyName> + Send + 'static;

        type Key: ?Sized + ToOwned<Owned = Self::OwnedKey>;
        type OwnedKey: Borrow<Self::Key> + Send + 'static;
        type KeyBytes<'a>: AsRef<[u8]>;

        type Value: ?Sized + ToOwned<Owned = Self::OwnedValue>;
        type OwnedValue: Borrow<Self::Value> + Send + 'static;
        type ValueBytes<'a>: AsRef<[u8]>;

        type ParseError: Into<anyhow::Error>;

        const CF_NAME: &'static str;
        const VALUE_NAME: &'static str;

        fn key_bytes(key: &Self::Key) -> Self::KeyBytes<'_>;
        fn value_bytes(value: &Self::Value) -> Self::ValueBytes<'_>;
        fn parse_value(bytes: Vec<u8>) -> Result<Self::OwnedValue, Self::ParseError>;
    }
}

use private::CacheFamily;

#[derive(Clone)]
pub struct Cache {
    dir: PathBuf,
    config: CacheConfig,
    db: Arc<DB>,
}

// NB: The async methods all have a bunch of overhead - they are all analogous
//     to calling the blocking versions inside spawn_blocking, and should only
//     be used in contexts where spawn_blocking would be otherwise required.

#[allow(dead_code)]
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

    pub async fn get<C: CacheFamily>(&self) -> Result<CacheRef<'static, C>> {
        let this = self.clone();
        spawn_blocking(move || {
            this.create_cf(C::CF_NAME)?;

            Ok(CacheRef(Cow::Owned(this), PhantomData))
        })
        .await
        .unwrap()
    }

    pub fn get_sync<C: CacheFamily>(&self) -> Result<CacheRef<C>> {
        self.create_cf(C::CF_NAME)?;

        Ok(CacheRef(Cow::Borrowed(self), PhantomData))
    }
}

#[repr(transparent)]
pub struct CacheRef<'a, F>(Cow<'a, Cache>, PhantomData<F>);

#[allow(dead_code)]
impl<'a, F: CacheFamily> CacheRef<'a, F> {
    #[inline]
    fn to_static(&self) -> CacheRef<'static, F> {
        CacheRef(Cow::Owned(self.0.as_ref().to_owned()), PhantomData)
    }

    #[inline]
    fn get_cf(&'a self) -> Result<ColumnFamilyRef> { self.0.get_cf(F::CF_NAME) }

    #[inline]
    pub async fn get_named(
        &self,
        name: F::OwnedKeyName,
        key: F::OwnedKey,
    ) -> Result<Option<F::OwnedValue>> {
        let this = self.to_static();
        spawn_blocking(move || this.get_named_sync(name, key))
            .await
            .unwrap()
    }

    pub fn get_named_sync(
        &self,
        name: impl Borrow<F::KeyName>,
        key: impl Borrow<F::Key>,
    ) -> Result<Option<F::OwnedValue>> {
        let name = name.borrow();
        let bytes = self
            .0
            .db
            .get_cf_opt(
                &self.get_cf()?,
                F::key_bytes(key.borrow()),
                self.0.config.read_opts(),
            )
            .with_context(|| format!("Error getting {} for {name:?}", F::VALUE_NAME))?;

        let Some(bytes) = bytes else { return Ok(None) };
        let parsed = F::parse_value(bytes).map_err(Into::into).with_context(|| {
            format!(
                "Error parsing {} for {name:?} (this should not happen)",
                F::VALUE_NAME
            )
        })?;

        Ok(Some(parsed))
    }

    #[inline]
    pub async fn set_named(
        &self,
        name: F::OwnedKeyName,
        key: F::OwnedKey,
        value: F::OwnedValue,
    ) -> Result<()> {
        let this = self.to_static();
        spawn_blocking(move || this.set_named_sync(name, key, value))
            .await
            .unwrap()
    }

    #[inline]
    pub fn set_named_sync(
        &self,
        name: impl Borrow<F::KeyName>,
        key: impl Borrow<F::Key>,
        value: impl Borrow<F::Value>,
    ) -> Result<()> {
        let name = name.borrow();
        let bytes = F::value_bytes(value.borrow());

        self.0
            .db
            .put_cf_opt(
                &self.get_cf()?,
                F::key_bytes(key.borrow()).as_ref(),
                bytes.as_ref(),
                self.0.config.write_opts(),
            )
            .with_context(|| format!("Error setting {} for {name:?}", F::VALUE_NAME))
    }
}

#[allow(dead_code)]
impl<'a, F: CacheFamily> CacheRef<'a, F>
where F::Key: Borrow<F::KeyName>
{
    #[inline]
    pub fn get_sync(&self, key: impl Borrow<F::Key>) -> Result<Option<F::OwnedValue>> {
        let key = key.borrow();
        self.get_named_sync(key.borrow(), key)
    }

    #[inline]
    pub fn set_sync(&self, key: impl Borrow<F::Key>, value: impl Borrow<F::Value>) -> Result<()> {
        let key = key.borrow();
        self.set_named_sync(key.borrow(), key, value)
    }
}

#[allow(dead_code)]
impl<'a, F: CacheFamily> CacheRef<'a, F>
where F::OwnedKey: ToOwned<Owned = F::OwnedKeyName>
{
    #[inline]
    pub async fn get(&self, key: F::OwnedKey) -> Result<Option<F::OwnedValue>> {
        self.get_named(key.to_owned(), key).await
    }

    #[inline]
    pub async fn set(&self, key: F::OwnedKey, value: F::OwnedValue) -> Result<()> {
        self.set_named(key.to_owned(), key, value).await
    }
}

pub struct AirdropWalletsCacheFamily;
pub type AirdropWalletsCache<'a> = CacheRef<'a, AirdropWalletsCacheFamily>;

#[derive(Clone, Copy)]
pub struct AirdropId {
    pub wallet: Pubkey,
    pub nft_number: u32,
}

impl AirdropId {
    pub fn to_bytes(self) -> [u8; 36] {
        let mut buf = [0_u8; 36];

        let (bw, bn) = buf.split_at_mut(32);
        bw.copy_from_slice(&self.wallet.to_bytes());
        bn.copy_from_slice(&self.nft_number.to_le_bytes());

        buf
    }
}

impl fmt::Debug for AirdropId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { wallet, nft_number } = self;
        write!(f, "NFT #{nft_number} for {wallet}")
    }
}

impl CacheFamily for AirdropWalletsCacheFamily {
    type KeyName = AirdropId;
    type OwnedKeyName = AirdropId;

    type Key = AirdropId;
    type OwnedKey = AirdropId;
    type KeyBytes<'a> = [u8; 36];

    type Value = MintRandomQueued;
    type OwnedValue = MintRandomQueued;
    type ValueBytes<'a> = Vec<u8>;

    type ParseError = prost::DecodeError;

    const CF_NAME: &'static str = "airdrop-wallets";
    const VALUE_NAME: &'static str = "airdrop mint status";

    #[inline]
    fn key_bytes(key: &Self::Key) -> Self::KeyBytes<'_> { key.to_bytes() }

    #[inline]
    fn value_bytes(value: &Self::Value) -> Self::ValueBytes<'_> { value.encode_to_vec() }

    #[inline]
    fn parse_value(bytes: Vec<u8>) -> Result<Self::OwnedValue, Self::ParseError> {
        Self::OwnedValue::decode(&*bytes)
    }
}

pub struct AssetUploadCacheFamily;
pub type AssetUploadCache<'a> = CacheRef<'a, AssetUploadCacheFamily>;

impl CacheFamily for AssetUploadCacheFamily {
    type KeyName = Path;
    type OwnedKeyName = PathBuf;

    type Key = Checksum;
    type OwnedKey = Checksum;
    type KeyBytes<'a> = [u8; 16];

    type Value = AssetUpload;
    type OwnedValue = AssetUpload;
    type ValueBytes<'a> = Vec<u8>;

    type ParseError = prost::DecodeError;

    const CF_NAME: &'static str = "asset-uploads";
    const VALUE_NAME: &'static str = "asset upload";

    #[inline]
    fn key_bytes(key: &Self::Key) -> Self::KeyBytes<'_> { key.to_bytes() }

    #[inline]
    fn value_bytes(value: &Self::Value) -> Self::ValueBytes<'_> { value.encode_to_vec() }

    #[inline]
    fn parse_value(bytes: Vec<u8>) -> Result<Self::OwnedValue, Self::ParseError> {
        Self::OwnedValue::decode(&*bytes)
    }
}

pub struct DropMintCacheFamily;
pub type DropMintCache<'a> = CacheRef<'a, DropMintCacheFamily>;

impl CacheFamily for DropMintCacheFamily {
    type KeyName = Path;
    type OwnedKeyName = PathBuf;

    type Key = Checksum;
    type OwnedKey = Checksum;
    type KeyBytes<'a> = [u8; 16];

    type Value = DropMint;
    type OwnedValue = DropMint;
    type ValueBytes<'a> = Vec<u8>;

    type ParseError = prost::DecodeError;

    const CF_NAME: &'static str = "drop-mints";
    const VALUE_NAME: &'static str = "drop mint";

    #[inline]
    fn key_bytes(key: &Self::Key) -> Self::KeyBytes<'_> { key.to_bytes() }

    #[inline]
    fn value_bytes(value: &Self::Value) -> Self::ValueBytes<'_> { value.encode_to_vec() }

    #[inline]
    fn parse_value(bytes: Vec<u8>) -> Result<Self::OwnedValue, Self::ParseError> {
        Self::OwnedValue::decode(&*bytes)
    }
}
