use std::future::Future;

use futures_util::{stream::FuturesUnordered, StreamExt};

pub async fn try_run<
    F: Future<Output = Result<(), E>>,
    E,
    G: FnMut() -> Result<Option<F>, E>,
    H: FnMut(E),
>(
    jobs: usize,
    mut err: H,
    mut get_job: G,
) -> Result<(), E> {
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
