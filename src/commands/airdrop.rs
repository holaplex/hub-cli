use core::fmt;
use std::{
    borrow::Cow,
    fs::File,
    io,
    io::BufRead,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread::sleep,
    time::Duration,
};

use anyhow::{bail, Context as _, Result};
use crossbeam::channel::{self, Sender};
use futures_util::FutureExt;
use graphql_client::GraphQLQuery;
use log::{error, info, warn};
use tokio::task::JoinHandle;
use url::Url;

use crate::{
    cache::{AirdropWalletsCache, Cache, CacheConfig, CreationStatus, ProtoMintRandomQueued},
    cli::Airdrop,
    commands::airdrop::mint_random_queued_to_drop::{
        MintRandomQueuedInput, MintRandomQueuedToDropMintRandomQueuedToDropCollectionMint,
    },
    common::{concurrent, error::format_errors, reqwest::ResponseExt, tokio::runtime},
    config::Config,
};

#[allow(clippy::upper_case_acronyms)]
type UUID = uuid::Uuid;

struct Pubkey([u8; 32]);

impl fmt::Display for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", bs58::encode(&self.0).into_string())
    }
}

impl TryFrom<&str> for Pubkey {
    type Error = anyhow::Error;

    fn try_from(s: &str) -> Result<Pubkey> {
        let mut pubkey_bytes: [u8; 32] = [0; 32];
        bs58::decode(&s)
            .onto(&mut pubkey_bytes)
            .context("failed to parse string as pubkey")
            .map(|_| Self(pubkey_bytes))
    }
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/queries/schema.graphql",
    query_path = "src/queries/mint-random.graphql",
    response_derives = "Debug"
)]
struct MintRandomQueuedToDrop;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/queries/schema.graphql",
    query_path = "src/queries/mint-status.graphql",
    response_derives = "Debug"
)]
struct MintStatus;

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum Job {
    Mint(MintRandomQueued),
    CheckStatus(CheckMintStatus),
    // RetryFailed(RetryFailedMint),
}

impl Job {
    #[inline]
    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        match self {
            Job::Mint(j) => j.run(ctx),
            Job::CheckStatus(j) => j.run(ctx),
        }
    }
}

#[derive(Default)]
struct Stats {
    pending_mints: AtomicUsize,
    failed_mints: AtomicUsize,
    created_mints: AtomicUsize,
}

#[derive(Debug)]
struct CheckMintStatus {
    path: PathBuf,
    mint_id: UUID,
    key: Cow<'static, str>,
}

impl CheckMintStatus {
    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let Self { path, mint_id, key } = self;
            let cache: AirdropWalletsCache = ctx.cache.get().await?;
            let wallet = key.split_once(':').unwrap().0;

            let input = mint_status::Variables { id: mint_id };

            let res = ctx
                .client
                .post(ctx.graphql_endpoint)
                .json(&MintStatus::build_query(input))
                .send()
                .await
                .error_for_hub_status(|| format!("check mint status mutation for {path:?}"))?
                .json::<graphql_client::Response<<MintStatus as GraphQLQuery>::ResponseData>>()
                .await
                .with_context(|| format!("Error parsing mutation response for {path:?}"))?;

            if let Some(data) = res.data {
                format_errors(res.errors, (), |s| {
                    warn!("checkMintStatus mutation for {path:?} returned one or more errors:{s}");
                });

                info!("Checking status for mint {mint_id:?}");

                let response = data.mint.context("Mint not found")?;
                let mint_status::MintStatusMint {
                    id,
                    creation_status,
                } = response;

                // impl From trait

                match creation_status {
                    mint_status::CreationStatus::CREATED => {
                        info!("Mint {mint_id:?} airdropped to {wallet:?}");
                        ctx.stats.pending_mints.fetch_sub(1, Ordering::Relaxed);
                        ctx.stats.created_mints.fetch_add(1, Ordering::Relaxed);

                        cache.set_sync(path.clone(), &key, &ProtoMintRandomQueued {
                            mint_id: id.to_string(),
                            mint_address: None,
                            status: CreationStatus::Created.into(),
                        })?;
                    },
                    mint_status::CreationStatus::PENDING => {
                        sleep(Duration::from_secs(3));
                        ctx.q.send(Job::CheckStatus(CheckMintStatus {
                            path: path.clone(),
                            mint_id: id,
                            key: key.clone(),
                        }))?;
                    },
                    _ => {
                        ctx.stats.failed_mints.fetch_add(1, Ordering::Relaxed);
                        info!("Mint {mint_id:?}:? failed. retrying..");
                        cache.set_sync(path.clone(), &key, &ProtoMintRandomQueued {
                            mint_id: id.to_string(),
                            mint_address: None,
                            status: CreationStatus::Failed.into(),
                        })?;
                    },
                }
            } else {
                format_errors(res.errors, Ok(()), |s| {
                    bail!("checkMintStatus mutation for {path:?} returned one or more errors:{s}")
                })?;

                bail!("CheckMintStatus mutation for {path:?} returned no data");
            }

            Ok(())
        })
    }
}

#[derive(Debug)]
struct MintRandomQueued {
    path: PathBuf,
    key: Cow<'static, str>,
    drop_id: UUID,
    compressed: bool,
}

impl MintRandomQueued {
    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        tokio::task::spawn(async move {
            let Self {
                path,
                key,
                drop_id,
                compressed,
            } = self;
            let cache: AirdropWalletsCache = ctx.cache.get().await?;

            let record = cache.get_sync(path.clone(), key.to_string())?;
            if let Some(r) = record {
                let status =
                    CreationStatus::try_from(r.status).context("invalid creation status")?;
                let wallet = key.split_once(':').unwrap().0;
                match status {
                    CreationStatus::Created => {
                        info!("Mint {:?} already airdropped to {wallet:?}", r.mint_id,);
                        return Ok(());
                    },
                    CreationStatus::Failed => {
                        //retry here

                        info!("Mint {:?} failed. retrying..", r.mint_id,);
                    },

                    CreationStatus::Pending => {
                        info!("Mint {:?} is pending. Checking status again...", r.mint_id,);
                        ctx.q.send(Job::CheckStatus(CheckMintStatus {
                            path,
                            mint_id: r.mint_id.parse()?,
                            key,
                        }))?;
                    },
                }
            } else {
                let (wallet, _) = key.split_once(':').unwrap();
                let input = mint_random_queued_to_drop::Variables {
                    in_: MintRandomQueuedInput {
                        drop: drop_id,
                        recipient: wallet.to_string(),
                        compressed,
                    },
                };

                let res =
                    ctx.client
                        .post(ctx.graphql_endpoint)
                        .json(&MintRandomQueuedToDrop::build_query(input))
                        .send()
                        .await
                        .error_for_hub_status(|| format!("queueMintToDrop mutation for {path:?}"))?
                        .json::<graphql_client::Response<
                            <MintRandomQueuedToDrop as GraphQLQuery>::ResponseData,
                        >>()
                        .await
                        .with_context(|| format!("Error parsing mutation response for {path:?}"))?;

                log::trace!("GraphQL response for {path:?}: {res:?}");

                if let Some(data) = res.data {
                    format_errors(res.errors, (), |s| {
                        warn!(
                            "mintRandomQueuedToDrop mutation for {path:?} returned one or more \
                             errors:{s}"
                        );
                    });

                    let mint_random_queued_to_drop::ResponseData {
                        mint_random_queued_to_drop:
                            mint_random_queued_to_drop::MintRandomQueuedToDropMintRandomQueuedToDrop {
                                collection_mint:
                                    MintRandomQueuedToDropMintRandomQueuedToDropCollectionMint {
                                        id,
                                        ..
                                    },
                            },
                    } = data;

                    cache.set_sync(path.clone(), &key, &ProtoMintRandomQueued {
                        mint_id: id.to_string(),
                        mint_address: None,
                        status: CreationStatus::Pending.into(),
                    })?;

                    ctx.stats.pending_mints.fetch_add(1, Ordering::Relaxed);

                    ctx.q.send(Job::CheckStatus(CheckMintStatus {
                        path,
                        mint_id: id,
                        key: key.clone(),
                    }))?;

                    info!("Pending for wallet {wallet:?}");
                } else {
                    format_errors(res.errors, Ok(()), |s| {
                        bail!(
                            "mintRandomQueuedToDrop mutation for {path:?} returned one or more \
                             errors:{s}"
                        )
                    })?;

                    ctx.stats.failed_mints.fetch_add(1, Ordering::Relaxed);

                    bail!("mintRandomQueuedToDrop mutation for {path:?} returned no data");
                }
            }

            Ok(())
        })
    }
}

#[derive(Clone)]
struct Context {
    graphql_endpoint: Url,
    client: reqwest::Client,
    q: Sender<Job>,
    cache: Cache,
    stats: Arc<Stats>,
}

pub fn run(config: &Config, cache: CacheConfig, args: Airdrop) -> Result<()> {
    let mut any_errs = false;

    let Airdrop {
        drop_id,
        wallets,
        compressed,
        mints_per_wallet,
        jobs,
    } = args;

    let (tx, rx) = channel::unbounded();

    let input_file = File::open(&wallets)
        .with_context(|| format!("wallets file does not exist {:?}", wallets))?;
    let reader = io::BufReader::new(input_file);

    for line in reader.lines() {
        let line = line?;
        let pubkey: Pubkey = line.trim_end_matches('\n').try_into()?;

        let mut nft_number = 1;
        while nft_number <= mints_per_wallet {
            let key: String = format!("{pubkey}:{nft_number}");
            tx.send(Job::Mint(MintRandomQueued {
                key: Cow::Owned(key),
                drop_id,
                path: drop_id.to_string().into(),
                compressed,
            }))
            .context("Error seeding initial job queue")?;
            nft_number += 1;
        }
    }

    let ctx = Context {
        graphql_endpoint: config.graphql_endpoint().clone(),
        client: config.graphql_client()?,
        q: tx,
        cache: Cache::load_sync(Path::new(".airdrops").join(drop_id.to_string()), cache)?,
        stats: Arc::default(),
    };

    runtime()?.block_on(async move {
        let res = concurrent::try_run(
            jobs,
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

                log::trace!("Submitting job: {job:?}");

                Ok(Some(job.run(ctx.clone()).map(|f| {
                    f.context("Worker task panicked").and_then(|r| r)
                })))
            },
        )
        .await;

        debug_assert!(rx.is_empty(), "Trailing jobs in queue");

        let Stats {
            created_mints,
            pending_mints,
            failed_mints,
        } = &*ctx.stats;
        info!(
            "Created: {:?}  Pending: {:?}   Failed: {:?}",
            created_mints.load(Ordering::Relaxed),
            pending_mints.load(Ordering::Relaxed),
            failed_mints.load(Ordering::Relaxed),
        );

        res
    })?;

    Ok(())
}
