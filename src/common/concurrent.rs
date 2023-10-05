use std::{fmt, future::Future};

use anyhow::Context;
use crossbeam::channel;
use futures_util::{stream::FuturesUnordered, FutureExt, StreamExt};

use crate::cli::ConcurrencyOpts;

pub async fn try_run<
    F: Future<Output = Result<(), E>>,
    E,
    G: FnMut() -> Result<Option<F>, E>,
    H: FnMut(E),
>(
    opts: ConcurrencyOpts,
    mut err: H,
    mut get_job: G,
) -> Result<(), E> {
    let ConcurrencyOpts { jobs } = opts;
    let jobs = jobs.into();
    let mut futures = FuturesUnordered::new();

    'run: loop {
        'pull: while futures.len() < jobs {
            let Some(job) = get_job()? else {
                let false = futures.is_empty() else {
                    break 'run;
                };

                break 'pull;
            };

            futures.push(job);
        }

        match futures.next().await {
            None | Some(Ok(())) => (),
            Some(Err(e)) => err(e),
        }
    }

    debug_assert!(futures.is_empty(), "Not all futures were yielded");

    Ok(())
}

#[inline]
pub async fn try_run_channel<
    J: fmt::Debug,
    F: FnMut(J) -> tokio::task::JoinHandle<Result<(), anyhow::Error>>,
>(
    opts: ConcurrencyOpts,
    rx: channel::Receiver<J>,
    mut run: F,
) -> (Result<(), anyhow::Error>, bool) {
    let mut any_errs = false;
    let res = try_run(
        opts,
        |e| {
            log::error!("{e:?}");
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

            Ok(Some(run(job).map(|f| {
                f.context("Worker task panicked").and_then(|r| r)
            })))
        },
    )
    .await;

    debug_assert!(rx.is_empty(), "Trailing jobs in queue");

    (res, any_errs)
}
