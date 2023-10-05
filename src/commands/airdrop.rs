use std::{
    fmt,
    fs::File,
    io,
    io::{prelude::*, BufReader},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context as _, Result};
use backon::{BackoffBuilder, ExponentialBackoff, ExponentialBuilder};
use crossbeam::channel::{self, Sender};
use graphql_client::GraphQLQuery;
use log::{info, warn};
use tokio::task::JoinHandle;
use url::Url;
use uuid::Uuid;

use self::mint_random_queued_to_drop::{
    MintRandomQueuedInput, MintRandomQueuedToDropMintRandomQueuedToDropCollectionMint,
};
use crate::{
    cache::{AirdropId, AirdropWalletsCache, Cache, CacheConfig, CreationStatus, MintRandomQueued},
    cli::Airdrop,
    common::{
        concurrent, graphql::UUID, pubkey::Pubkey, reqwest::ClientExt, stats::Counter,
        tokio::runtime,
    },
    config::Config,
};

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

#[derive(Default)]
struct Stats {
    queued_mints: Counter,
    pending_mints: Counter,
    failed_mints: Counter,
    created_mints: Counter,
}

#[derive(Clone)]
struct Params<'a> {
    drop_id: Uuid,
    compressed: bool,
    mints_per_wallet: u32,
    tx: &'a Sender<Job>,
    backoff: ExponentialBuilder,
}

fn read_file<N: fmt::Debug, R: BufRead>(name: N, reader: R, params: Params) -> Result<()> {
    let Params {
        drop_id,
        compressed,
        mints_per_wallet,
        tx,
        backoff,
    } = params;

    for line in reader.lines() {
        let wallet: Pubkey = line
            .map_err(anyhow::Error::new)
            .and_then(|l| l.trim().parse().map_err(Into::into))
            .with_context(|| format!("Error parsing wallets file {name:?}"))?;

        for nft_number in 1..=mints_per_wallet {
            tx.send(Job::Mint(MintRandomQueuedJob {
                airdrop_id: AirdropId { wallet, nft_number },
                drop_id,
                compressed,
                backoff: backoff.build(),
            }))
            .context("Error seeding initial job queue")?;
        }
    }

    Ok(())
}

pub fn run(config: &Config, cache: CacheConfig, args: Airdrop) -> Result<()> {
    let Airdrop {
        concurrency,
        drop_id,
        no_compressed,
        mints_per_wallet,
        wallets,
    } = args;

    let (tx, rx) = channel::unbounded();

    let params = Params {
        drop_id,
        compressed: !no_compressed,
        mints_per_wallet,
        tx: &tx,
        backoff: ExponentialBuilder::default()
            .with_jitter()
            .with_factor(2.0)
            .with_min_delay(Duration::from_secs(3))
            .with_max_times(5),
    };

    for path in wallets {
        let params = params.clone();

        if path.as_os_str() == "-" {
            read_file("STDIN", BufReader::new(io::stdin()), params)?;
        } else {
            read_file(
                &path,
                BufReader::new(
                    File::open(&path)
                        .with_context(|| format!("Error opening wallets file {path:?}"))?,
                ),
                params,
            )?;
        }
    }

    let ctx = Context {
        // TODO: what should the correct path for this be?
        cache: Cache::load_sync(Path::new(".airdrops").join(drop_id.to_string()), cache)?,
        graphql_endpoint: config.graphql_endpoint().clone(),
        client: config.graphql_client()?,
        q: tx,
        stats: Arc::default(),
    };

    runtime()?.block_on(async move {
        let (res, any_errs) =
            concurrent::try_run_channel(concurrency, rx, |j| j.run(ctx.clone())).await;

        let Stats {
            queued_mints,
            created_mints,
            pending_mints,
            failed_mints,
        } = &*ctx.stats;
        info!(
            "Of {queued} new mint(s) queued: {created} created, {pending} still pending, {failed} \
             failed",
            queued = queued_mints.load(),
            created = created_mints.load(),
            pending = pending_mints.load(),
            failed = failed_mints.load(),
        );

        if any_errs {
            warn!(
                "Some mints were skipped due to errors.  They will be processed next time this \
                 command is run."
            );
        }

        res
    })
}

#[derive(Clone)]
struct Context {
    cache: Cache,
    graphql_endpoint: Url,
    client: reqwest::Client,
    q: Sender<Job>,
    stats: Arc<Stats>,
}

#[derive(Debug)]
enum Job {
    Mint(MintRandomQueuedJob),
    CheckStatus(CheckMintStatusJob),
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

#[derive(Debug)]
struct MintRandomQueuedJob {
    airdrop_id: AirdropId,
    drop_id: Uuid,
    compressed: bool,
    backoff: ExponentialBackoff,
}

impl MintRandomQueuedJob {
    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        tokio::task::spawn(async move {
            let Self {
                airdrop_id,
                drop_id,
                compressed,
                backoff,
            } = self;
            let AirdropId {
                wallet,
                nft_number: _,
            } = airdrop_id;
            let cache: AirdropWalletsCache = ctx.cache.get().await?;

            let record = cache.get(airdrop_id).await?;
            if let Some(r) = record {
                let status = CreationStatus::try_from(r.status)
                    .with_context(|| format!("Missing creation status for {airdrop_id:?}"))?;

                match status {
                    CreationStatus::Created => {
                        info!("Mint {:?} already airdropped to {wallet:?}", r.mint_id,);
                        return Ok(());
                    },
                    CreationStatus::Failed => {
                        // TODO: retry here

                        warn!("Mint {:?} failed.", r.mint_id);
                    },
                    CreationStatus::Pending => {
                        info!("Mint {:?} is pending.  Checking status again...", r.mint_id);
                        ctx.q
                            .send(Job::CheckStatus(CheckMintStatusJob {
                                airdrop_id,
                                mint_id: r.mint_id.parse().with_context(|| {
                                    format!("Invalid mint ID for {airdrop_id:?}")
                                })?,
                                backoff,
                            }))
                            .context("Error submitting pending mint status check job")?;
                    },
                }
            } else {
                let input = mint_random_queued_to_drop::Variables {
                    in_: MintRandomQueuedInput {
                        drop: drop_id,
                        recipient: wallet.to_string(),
                        compressed,
                    },
                };

                let res = ctx
                    .client
                    .graphql::<MintRandomQueuedToDrop>()
                    .post(ctx.graphql_endpoint, input, || {
                        format!("mintRandomQueuedToDrop mutation for {airdrop_id:?}")
                    })
                    .await?;

                let mint_random_queued_to_drop::ResponseData {
                    mint_random_queued_to_drop:
                        mint_random_queued_to_drop::MintRandomQueuedToDropMintRandomQueuedToDrop {
                            collection_mint:
                                MintRandomQueuedToDropMintRandomQueuedToDropCollectionMint {
                                    id, ..
                                },
                        },
                } = res.data;

                ctx.stats.queued_mints.increment();

                cache
                    .set(airdrop_id, MintRandomQueued {
                        mint_id: id.to_string(),
                        mint_address: None,
                        status: CreationStatus::Pending.into(),
                    })
                    .await?;

                ctx.q
                    .send(Job::CheckStatus(CheckMintStatusJob {
                        airdrop_id,
                        mint_id: id,
                        backoff,
                    }))
                    .context("Error submitting mint status check job")?;

                info!("Pending for wallet {wallet:?}");
            }

            Ok(())
        })
    }
}

#[derive(Debug)]
struct CheckMintStatusJob {
    airdrop_id: AirdropId,
    mint_id: Uuid,
    backoff: ExponentialBackoff,
}

impl CheckMintStatusJob {
    fn run(self, ctx: Context) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let Self {
                airdrop_id,
                mint_id,
                mut backoff,
            } = self;
            let cache: AirdropWalletsCache = ctx.cache.get().await?;

            let res = ctx
                .client
                .graphql::<MintStatus>()
                .post(
                    ctx.graphql_endpoint,
                    mint_status::Variables { id: mint_id },
                    || format!("mint creationStatus query for {airdrop_id:?}"),
                )
                .await?;

            // TODO: review all logging calls to ensure we're outputting the
            //       correct amount of verbosity
            info!("Checking status for mint {mint_id:?}");

            let response = res
                .data
                .mint
                .with_context(|| format!("Mint not found for {airdrop_id:?}"))?;
            let mint_status::MintStatusMint {
                id,
                creation_status,
            } = response;

            match creation_status {
                mint_status::CreationStatus::CREATED => {
                    info!("Mint {mint_id:?} airdropped for {airdrop_id:?}");

                    ctx.stats.created_mints.increment();

                    cache
                        .set(airdrop_id, MintRandomQueued {
                            mint_id: id.to_string(),
                            mint_address: None,
                            status: CreationStatus::Created.into(),
                        })
                        .await?;
                },
                mint_status::CreationStatus::PENDING => {
                    let Some(dur) = backoff.next() else {
                        warn!("Timed out waiting for {airdrop_id:?} to complete");
                        ctx.stats.pending_mints.increment();

                        return Ok(());
                    };

                    tokio::time::sleep(dur).await;

                    ctx.q
                        .send(Job::CheckStatus(CheckMintStatusJob {
                            airdrop_id,
                            mint_id: id,
                            backoff,
                        }))
                        .context("Error submitting mint status check job")?;
                },
                _ => {
                    let Some(dur) = backoff.next() else {
                        ctx.stats.failed_mints.increment();
                        bail!("Mint {mint_id:?} for {airdrop_id:?} failed too many times");
                    };

                    warn!("Mint {mint_id:?} failed.");
                    tokio::time::sleep(dur).await;

                    cache
                        .set(airdrop_id, MintRandomQueued {
                            mint_id: id.to_string(),
                            mint_address: None,
                            status: CreationStatus::Failed.into(),
                        })
                        .await
                        .context("Error submitting mint retry job")?;
                },
            }

            Ok(())
        })
    }
}
