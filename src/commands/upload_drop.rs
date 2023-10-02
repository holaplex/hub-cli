use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt::Write,
    fs::{self, File},
    io::{self, prelude::*},
    iter,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
};

use anyhow::{bail, Context as _, Result};
use crossbeam::{
    channel::{self, Sender},
    queue::ArrayQueue,
};
use dashmap::DashMap;
use futures_util::FutureExt;
use graphql_client::GraphQLQuery;
use itertools::Either;
use log::{error, info, trace, warn};
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use url::Url;
use uuid::Uuid;

use crate::{
    cache::{AssetUpload, AssetUploadCache, Cache, CacheConfig, Checksum, DropMint, DropMintCache},
    cli::UploadDrop,
    common::{
        concurrent,
        metadata_json::{self, MetadataJson},
        tokio::runtime,
        toposort::{Dependencies, Dependency, PendingFail},
        url_permissive::PermissiveUrl,
    },
    config::Config,
};

type UploadResponse = Vec<UploadedAsset>;

#[derive(Debug, Serialize, Deserialize)]
struct UploadedAsset {
    name: String,
    url: Url,
}

#[allow(clippy::upper_case_acronyms)]
type UUID = Uuid;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/queries/schema.graphql",
    query_path = "src/queries/queue-mint-to-drop.graphql",
    variables_derives = "Debug, PartialEq, Eq, Hash",
    response_derives = "Debug"
)]
struct QueueMintToDrop;

impl From<MetadataJson> for queue_mint_to_drop::MetadataJsonInput {
    fn from(value: MetadataJson) -> Self {
        let MetadataJson {
            name,
            symbol,
            description,
            image,
            animation_url,
            collection,
            attributes,
            external_url,
            properties,
        } = value;
        Self {
            name,
            symbol,
            description,
            image: image.into(),
            animation_url: animation_url.map(Into::into),
            collection: collection.into(),
            attributes: attributes.into_iter().map(Into::into).collect(),
            external_url: external_url.map(Into::into),
            properties: properties.into(),
        }
    }
}

impl From<metadata_json::Collection> for Option<queue_mint_to_drop::MetadataJsonCollectionInput> {
    #[inline]
    fn from(value: metadata_json::Collection) -> Self {
        if value.is_empty() {
            None
        } else {
            let metadata_json::Collection { name, family } = value;
            Some(queue_mint_to_drop::MetadataJsonCollectionInput { name, family })
        }
    }
}

impl From<metadata_json::Attribute> for queue_mint_to_drop::MetadataJsonAttributeInput {
    #[inline]
    fn from(value: metadata_json::Attribute) -> Self {
        let metadata_json::Attribute { trait_type, value } = value;
        Self { trait_type, value }
    }
}

impl From<metadata_json::Properties> for Option<queue_mint_to_drop::MetadataJsonPropertyInput> {
    #[inline]
    fn from(value: metadata_json::Properties) -> Self {
        if value.is_empty() {
            None
        } else {
            let metadata_json::Properties { files, category } = value;
            Some(queue_mint_to_drop::MetadataJsonPropertyInput {
                files: if files.is_empty() {
                    None
                } else {
                    Some(files.into_iter().map(Into::into).collect())
                },
                category,
            })
        }
    }
}

impl From<metadata_json::File> for queue_mint_to_drop::MetadataJsonFileInput {
    fn from(value: metadata_json::File) -> Self {
        let metadata_json::File { uri, ty } = value;
        Self {
            uri: Some(uri.into()),
            file_type: ty,
        }
    }
}

#[derive(Default)]
struct Stats {
    uploaded_assets: AtomicUsize,
    queued_mints: AtomicUsize,
}

pub fn run(config: &Config, cache: CacheConfig, args: UploadDrop) -> Result<()> {
    let UploadDrop {
        drop_id,
        include_dirs,
        jobs,
        input_dirs,
    } = args;
    let include_dirs: HashSet<_> = include_dirs.into_iter().collect();

    let (tx, rx) = channel::unbounded();
    for (dir, path) in input_dirs.iter().flat_map(|d| match fs::read_dir(d) {
        Ok(r) => {
            trace!("Traversing directory {r:?}");

            Either::Left(r.filter_map(move |f| {
                let path = f
                    .with_context(|| format!("Error reading JSON directory {d:?}"))
                    .map(|f| {
                        let path = f.path();

                        if path.extension().map_or(false, |p| p == "json") {
                            Some(path)
                        } else {
                            None
                        }
                    })
                    .transpose()?;

                Some((d, path))
            }))
        },
        Err(e) => Either::Right(
            [(
                d,
                Err(e).context(format!("Error opening JSON directory {d:?}")),
            )]
            .into_iter(),
        ),
    }) {
        tx.send(Job::ScanJson(ScanJsonJob {
            dir: dir.clone(),
            path,
        }))
        .context("Error seeding initial job queue")?;
    }

    info!("Scanning {} JSON file(s)...", rx.len());

    let ctx = Context {
        cache_config: cache,
        caches: Arc::default(),
        include_dirs: include_dirs
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice()
            .into(),
        drop_id,
        graphql_endpoint: config.graphql_endpoint().clone(),
        upload_endpoint: config.upload_endpoint()?,
        client: config.graphql_client()?,
        q: tx,
        stats: Arc::default(),
    };

    let mut any_errs = false;
    runtime()?.block_on(async move {
        let res = concurrent::try_run(
            jobs.into(),
            |e| {
                error!("{e:?}");
                any_errs = true;
            },
            || {
                let job = match rx.try_recv() {
                    Ok(j) => Some(j),
                    Err(channel::TryRecvError::Empty) => None,
                    Err(e) => return Err(e).context("Error getting job from queue"),
                };

                let Some(job) = job else {
                    return Ok(None);
                };

                trace!("Submitting job: {job:?}");

                Ok(Some(job.run(ctx.clone()).map(|f| {
                    f.context("Worker task panicked").and_then(|r| r)
                })))
            },
        )
        .await;

        debug_assert!(rx.is_empty(), "Trailing jobs in queue");

        let Stats {
            uploaded_assets,
            queued_mints,
        } = &*ctx.stats;
        info!(
            "Uploaded {assets} asset(s) and queued {mints} mint(s)",
            assets = uploaded_assets.load(std::sync::atomic::Ordering::Relaxed),
            mints = queued_mints.load(std::sync::atomic::Ordering::Relaxed)
        );

        if any_errs {
            warn!(
                "Some files were skipped due to errors.  They will be processed next time this \
                 command is run."
            );
        }

        res
    })?;

    Ok(())
}

#[derive(Clone)]
struct Context {
    cache_config: CacheConfig,
    caches: Arc<DashMap<PathBuf, Cache>>,
    include_dirs: Arc<[PathBuf]>,
    drop_id: Uuid,
    graphql_endpoint: Url,
    upload_endpoint: Url,
    client: reqwest::Client,
    q: Sender<Job>,
    stats: Arc<Stats>,
}

impl Context {
    fn cache(&self, path: impl AsRef<Path>) -> Result<Cache> {
        let entry = self
            .caches
            .entry(path.as_ref().to_path_buf())
            .or_try_insert_with(|| Cache::load_sync(path, self.cache_config.clone()))?;

        Ok((*entry).clone())
    }

    fn resolve_file<F: FnMut(&PathBuf) -> io::Result<T>, T>(
        &self,
        mut open: F,
    ) -> Result<Option<(&PathBuf, T)>, (&PathBuf, io::Error)> {
        static NIL: OnceLock<PathBuf> = OnceLock::new();

        [NIL.get_or_init(PathBuf::new)]
            .into_iter()
            .chain(&*self.include_dirs)
            .find_map(|d| {
                let opened = match open(d) {
                    Ok(o) => o,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
                    Err(e) => return Some(Err((d, e))),
                };

                Some(Ok((d, opened)))
            })
            .transpose()
    }
}

#[derive(Debug)]
struct ScanJsonJob {
    dir: PathBuf,
    path: Result<PathBuf>,
}

impl ScanJsonJob {
    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        tokio::task::spawn_blocking(move || {
            let Self { dir, path } = self;
            let path = path?;
            let json_file = File::open(&path).with_context(|| format!("Error opening {path:?}"))?;

            let ck = Checksum::read_rewind(&path, &json_file)?;

            let json: MetadataJson = serde_json::from_reader(json_file)
                .with_context(|| format!("Error parsing {path:?}"))?;

            let mut seen = HashSet::new();
            let local_urls = json
                .files()
                .filter_map(|u| {
                    if !seen.insert(u) {
                        return None;
                    }

                    let url = u.clone();
                    trace!("{url:?} -> {:?}", url.to_file_path());
                    let path = url.to_file_path()?;
                    let (include_dir, file) = match ctx
                        .resolve_file(|d| File::open(d.join(&path)))
                        .map_err(|(d, e)| {
                            anyhow::Error::new(e)
                                .context(format!("Error opening {:?}", d.join(&path)))
                        })
                        .and_then(|f| f.with_context(|| format!("Unable to resolve path {path:?}")))
                    {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = include_dir.join(path);

                    let ty = if let Some(ty) =
                        json.properties.find_file(&url).and_then(|f| f.ty.clone())
                    {
                        ty.into()
                    } else {
                        static INFER: infer::Infer = infer::Infer::new();

                        let limit = file
                            .metadata()
                            .map(|m| {
                                usize::try_from(std::cmp::min(m.len(), 8192)).unwrap_or_default()
                                    + 1
                            })
                            .unwrap_or(0);
                        let mut bytes = Vec::with_capacity(limit);

                        match file
                            .take(8192)
                            .read_to_end(&mut bytes)
                            .with_context(|| format!("Error reading signature of {path:?}"))
                            .and_then(|_| {
                                INFER
                                    .get(&bytes)
                                    .with_context(|| format!("Cannot infer MIME type for {path:?}"))
                            }) {
                            Ok(t) => t.mime_type().into(),
                            Err(e) => return Some(Err(e)),
                        }
                    };

                    Some(Ok((url, include_dir.clone(), path, ty)))
                })
                .collect::<Vec<_>>();

            if let Some(dep_count) = NonZeroUsize::new(local_urls.len()) {
                let rewrites = Arc::new(ArrayQueue::new(dep_count.get()));
                let deps = Dependencies::new(dep_count, QueueJsonJob {
                    dir,
                    path,
                    ck,
                    json,
                    rewrites: Some(Arc::clone(&rewrites)),
                });

                debug_assert!(rewrites.capacity() == deps.len());
                for (res, dep) in local_urls.into_iter().zip(deps) {
                    ctx.q
                        .send(Job::UploadAsset(UploadAssetJob {
                            asset: res.map(|(source_url, dir, path, mime_type)| Asset {
                                source_url,
                                dir,
                                path,
                                mime_type,
                            }),
                            rewrites: Arc::clone(&rewrites),
                            dep,
                        }))
                        .context("Error submitting asset upload job")?;
                }
            } else {
                ctx.q
                    .send(Job::QueueJson(QueueJsonJob {
                        dir,
                        path,
                        ck,
                        json,
                        rewrites: None,
                    }))
                    .context("Error submitting JSON queue job")?;
            }

            Ok(())
        })
    }
}

struct FileRewrite {
    source_url: PermissiveUrl,
    dest_url: Url,
    mime_type: Cow<'static, str>,
}

type FileRewrites = Arc<ArrayQueue<FileRewrite>>;

#[derive(Debug)]
struct Asset {
    source_url: PermissiveUrl,
    dir: PathBuf,
    path: PathBuf,
    mime_type: Cow<'static, str>,
}

#[derive(Debug)]
struct UploadAssetJob {
    asset: Result<Asset>,
    rewrites: FileRewrites,
    dep: Dependency<QueueJsonJob>,
}

impl UploadAssetJob {
    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let Self {
                asset,
                rewrites,
                dep,
            } = self;
            let Asset {
                dir,
                path,
                mime_type,
                source_url,
            } = asset?;

            let mut file = tokio::fs::File::open(&path)
                .await
                .with_context(|| format!("Error opening {path:?}"))?;
            let ck = Checksum::read_rewind_async(&path, &mut file).await?;

            let cache: AssetUploadCache = ctx.cache(dir)?.get().await?;
            let cached_url = cache
                .get(path.clone(), ck)
                .await?
                .and_then(|AssetUpload { url }| {
                    Url::parse(&url)
                        .map_err(|e| warn!("Invalid cache URL {url:?}: {e}"))
                        .ok()
                });

            let dest_url;
            if let Some(url) = cached_url {
                trace!("Using cached URL {:?} for {path:?}", url.as_str());
                dest_url = url;
            } else {
                let name = path
                    .file_name()
                    .with_context(|| format!("Error resolving file name for {path:?}"))?
                    .to_string_lossy()
                    .into_owned();

                let mut uploads = ctx
                    .client
                    .post(ctx.upload_endpoint)
                    .multipart(
                        multipart::Form::new().part(
                            "upload",
                            multipart::Part::stream(file)
                                .file_name(name.clone())
                                .mime_str(&mime_type)
                                .with_context(|| {
                                    format!(
                                        "Invalid MIME type {:?} for {path:?}",
                                        mime_type.as_ref()
                                    )
                                })?,
                        ),
                    )
                    .send()
                    .await
                    .with_context(|| format!("Error sending POST request for {path:?}"))?
                    .error_for_status()
                    .with_context(|| format!("POST request for {path:?} returned an error"))?
                    .json::<UploadResponse>()
                    .await
                    .with_context(|| {
                        format!("Error deserializing upload response JSON for {path:?}")
                    })?
                    .into_iter();

                if uploads.len() > 1 {
                    warn!("Trailing values in response data for {path:?}");
                }

                let upload = uploads
                    .find(|u| u.name == name)
                    .with_context(|| format!("Missing upload response data for {path:?}"))?;

                ctx.stats.uploaded_assets.fetch_add(1, Ordering::Relaxed);
                info!("Successfully uploaded {path:?}");

                cache
                    .set(path.clone(), ck, AssetUpload {
                        url: upload.url.to_string(),
                    })
                    .await
                    .map_err(|e| warn!("{e:?}"))
                    .ok();

                dest_url = upload.url;
            }

            rewrites
                .push(FileRewrite {
                    source_url,
                    dest_url,
                    mime_type,
                })
                .unwrap_or_else(|_: FileRewrite| {
                    unreachable!("Too many file rewrites for {path:?}")
                });

            dep.ok(|j| ctx.q.send(Job::QueueJson(j)))
                .transpose()
                .context("Error submitting JSON queue job")?;

            Ok(())
        })
    }
}

#[derive(Debug)]
struct QueueJsonJob {
    dir: PathBuf,
    path: PathBuf,
    ck: Checksum,
    json: MetadataJson,
    rewrites: Option<FileRewrites>,
}

impl QueueJsonJob {
    fn format_errors<T>(
        errors: Option<Vec<graphql_client::Error>>,
        ok: T,
        f: impl FnOnce(String) -> T,
    ) -> T {
        let mut errs = errors.into_iter().flatten().peekable();

        if errs.peek().is_some() {
            let mut s = String::new();

            for err in errs {
                write!(s, "\n  {err}").unwrap();
            }

            f(s)
        } else {
            ok
        }
    }

    fn rewrite_json(json: &mut MetadataJson, rewrites: Option<Arc<ArrayQueue<FileRewrite>>>) {
        let rewrites: HashMap<_, _> = rewrites
            .into_iter()
            .flat_map(|r| iter::from_fn(move || r.pop()))
            .map(
                |FileRewrite {
                     source_url,
                     dest_url,
                     mime_type,
                 }| (source_url, (dest_url, mime_type)),
            )
            .collect();

        for file in json.files_mut() {
            if let Some((url, _)) = rewrites.get(file) {
                *file = PermissiveUrl::Url(url.clone());
            }
        }

        let seen_files: HashMap<_, _> = json
            .properties
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.uri.clone(), i))
            .collect();

        for (uri, ty) in rewrites.into_values() {
            let uri = PermissiveUrl::Url(uri);
            if let Some(idx) = seen_files.get(&uri) {
                json.properties.files[*idx].ty = Some(ty.into_owned());
            } else {
                json.properties.files.push(metadata_json::File {
                    uri,
                    ty: Some(ty.into_owned()),
                });
            }
        }
    }

    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let Self {
                dir,
                ck,
                path,
                mut json,
                rewrites,
            } = self;
            let cache: DropMintCache = ctx.cache(dir)?.get().await?;

            Self::rewrite_json(&mut json, rewrites);

            let input = queue_mint_to_drop::Variables {
                in_: queue_mint_to_drop::QueueMintToDropInput {
                    drop: ctx.drop_id,
                    metadata_json: json.into(),
                },
            };
            let input_ck = Checksum::hash(&input);

            trace!(
                "GraphQL input for {path:?}: {}",
                serde_json::to_string(&input).map_or_else(|e| e.to_string(), |j| j.to_string())
            );

            let cached_mint = cache.get(path.clone(), ck).await?;

            if let Some(mint) = cached_mint {
                trace!("Using cached mint {mint:?} for {path:?}");

                if mint.input_hash != input_ck.to_bytes() {
                    trace!(
                        "Hash mismatch for {path:?}: {:032x} cached vs {input_ck:?} new",
                        u128::from_le_bytes(mint.input_hash.try_into().unwrap_or_default())
                    );
                    warn!(
                        "Detected a change in {path:?} since it was uploaded - this will be \
                         ignored!"
                    );
                }
            } else {
                let res = ctx
                .client
                .post(ctx.graphql_endpoint)
                .json(&QueueMintToDrop::build_query(input))
                .send()
                .await
                .with_context(|| format!("Error sending queueMintToDrop mutation for {path:?}"))?
                .error_for_status()
                .with_context(|| {
                    format!("queueMintToDrop mutation for {path:?} returned an error")
                })?
                .json::<graphql_client::Response<<QueueMintToDrop as GraphQLQuery>::ResponseData>>()
                .await
                .with_context(|| {
                    format!("Error parsing queueMintToDrop mutation response for {path:?}")
                })?;

                trace!("GraphQL response for {path:?}: {res:?}");

                let collection_mint;
                if let Some(data) = res.data {
                    Self::format_errors(res.errors, (), |s| {
                        warn!(
                            "queueMintToDrop mutation for {path:?} returned one or more errors:{s}"
                        );
                    });

                    let queue_mint_to_drop::ResponseData {
                        queue_mint_to_drop:
                            queue_mint_to_drop::QueueMintToDropQueueMintToDrop {
                                collection_mint:
                                    queue_mint_to_drop::QueueMintToDropQueueMintToDropCollectionMint {
                                        id,
                                    },
                            },
                    } = data;
                    collection_mint = id;

                    ctx.stats.queued_mints.fetch_add(1, Ordering::Relaxed);
                    info!("Mint successfully queued for {path:?}");
                } else {
                    Self::format_errors(res.errors, Ok(()), |s| {
                        bail!(
                            "queueMintToDrop mutation for {path:?} returned one or more errors:{s}"
                        )
                    })?;

                    bail!("queueMintToDrop mutation for {path:?} returned no data");
                }

                cache
                    .set(path, ck, DropMint {
                        collection_mint: collection_mint.to_bytes_le().into(),
                        input_hash: input_ck.to_bytes().into(),
                    })
                    .await
                    .map_err(|e| warn!("{e:?}"))
                    .ok();
            }

            Ok(())
        })
    }
}

impl PendingFail for QueueJsonJob {
    fn failed(self) {
        warn!("Skipping {:?} due to failed dependencies", self.path);
    }
}

// The cost of shuffling these around is probably less than the cost of allocation
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum Job {
    ScanJson(ScanJsonJob),
    UploadAsset(UploadAssetJob),
    QueueJson(QueueJsonJob),
}

impl Job {
    #[inline]
    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        match self {
            Job::ScanJson(j) => j.run(ctx),
            Job::UploadAsset(j) => j.run(ctx),
            Job::QueueJson(j) => j.run(ctx),
        }
    }
}
