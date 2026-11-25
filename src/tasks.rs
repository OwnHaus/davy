//! Background work in `davy serve` that polls SQLite for due items.

use std::{collections::HashMap, future::Future, hash::Hash, time::Duration};
use tokio::{task::JoinSet, time::MissedTickBehavior};

/// Every `period`, reads the due items and spawns `work` for each, at most
/// `limit` at a time and one per key. A key stays busy until its work ends,
/// so work that stores its result before it ends is never started twice for
/// the same item. A failed read is reported and tried on the next tick.
/// Runs until aborted, which aborts the work in flight.
pub async fn poll<T, K, Fut>(
    name: &'static str,
    period: Duration,
    limit: usize,
    mut due: impl FnMut() -> anyhow::Result<Vec<T>>,
    key: impl Fn(&T) -> K,
    work: impl Fn(T) -> Fut,
) where
    K: Eq + Hash,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut busy = HashMap::new();
    let mut running = JoinSet::new();
    loop {
        tokio::select! {
            _ = ticks.tick() => {
                let items = match due() {
                    Ok(items) => items,
                    Err(e) => {
                        report(name, &e);
                        continue;
                    }
                };
                for item in items {
                    if busy.len() >= limit {
                        break;
                    }
                    let k = key(&item);
                    if busy.values().any(|b| *b == k) {
                        continue;
                    }
                    let task = running.spawn(work(item));
                    busy.insert(task.id(), k);
                }
            }
            Some(done) = running.join_next_with_id() => {
                let id = match done {
                    Ok((id, ())) => id,
                    Err(e) => e.id(),
                };
                busy.remove(&id);
            }
        }
    }
}

fn report(name: &'static str, e: &anyhow::Error) {
    crate::self_report::error(name, format_args!("reading due work: {e:#}"));
}

#[cfg(test)]
mod tests {
    use super::poll;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    /// Polls `items` (key, item) for `ms` with work that never ends, and
    /// returns the most that ran at once and how often each item started.
    async fn run(limit: usize, items: Vec<(u8, u8)>, ms: u64) -> (usize, Vec<usize>) {
        let state = Arc::new(Mutex::new((0, 0, vec![0; items.len()])));
        let work_state = Arc::clone(&state);
        let poller = tokio::spawn(async move {
            poll(
                "test",
                Duration::from_millis(5),
                limit,
                move || Ok(items.clone()),
                |&(key, _)| key,
                move |(_, item)| {
                    let state = Arc::clone(&work_state);
                    async move {
                        {
                            let mut s = state.lock().unwrap();
                            s.0 += 1;
                            s.1 = s.1.max(s.0);
                            s.2[usize::from(item)] += 1;
                        }
                        std::future::pending::<()>().await;
                    }
                },
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(ms)).await;
        poller.abort();
        assert!(poller.await.unwrap_err().is_cancelled());
        let s = state.lock().unwrap();
        (s.1, s.2.clone())
    }

    #[tokio::test]
    async fn no_more_than_the_limit_runs_at_once() {
        let (most, runs) = run(2, vec![(0, 0), (1, 1), (2, 2), (3, 3)], 20).await;
        assert_eq!(most, 2);
        assert_eq!(runs, [1, 1, 0, 0]);
    }

    #[tokio::test]
    async fn one_key_runs_one_item_at_a_time() {
        let (most, runs) = run(8, vec![(7, 0), (7, 1)], 20).await;
        assert_eq!(most, 1);
        assert_eq!(runs, [1, 0]);
    }
}
