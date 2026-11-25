use super::*;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

fn day(s: &str) -> Date {
    s.parse().unwrap()
}

/// A walk that counts its runs in `walks` and measures `bytes` of today.
fn walk(
    walks: &Arc<AtomicU64>,
    bytes: u64,
) -> impl FnOnce() -> Result<Disk, String> + Send + 'static {
    let walks = Arc::clone(walks);
    move || {
        walks.fetch_add(1, Ordering::SeqCst);
        Ok(Disk {
            today: bytes,
            ..Disk::default()
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_measurement_answers_for_a_minute_of_the_same_day() {
    let shared = Shared::default();
    let walks = Arc::new(AtomicU64::new(0));
    let today = day("2026-11-24");
    let secs = |s| tokio::time::advance(Duration::from_secs(s));
    let first = shared.get(today, walk(&walks, 1)).await;
    secs(59).await;
    let cached = shared.get(today, walk(&walks, 2)).await;
    secs(2).await;
    let stale = shared.get(today, walk(&walks, 3)).await;
    secs(1).await;
    let next_day = shared.get(day("2026-11-25"), walk(&walks, 4)).await;
    let today_of = |d: Result<Disk, String>| d.unwrap().today;
    assert_eq!(
        (
            today_of(first),
            today_of(cached),
            today_of(stale),
            today_of(next_day)
        ),
        (1, 1, 3, 4)
    );
    assert_eq!(walks.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_failed_walk_answers_for_its_minute_too() {
    let shared = Shared::default();
    let today = day("2026-11-24");
    let failed = shared.get(today, || Err("opening /data".to_owned())).await;
    let walks = Arc::new(AtomicU64::new(0));
    let again = shared.get(today, walk(&walks, 1)).await;
    assert_eq!(
        (failed, again),
        (
            Err("opening /data".to_owned()),
            Err("opening /data".to_owned())
        )
    );
    assert_eq!(walks.load(Ordering::SeqCst), 0);
}

#[test]
fn callers_during_a_walk_share_it() {
    // One thread runs every caller, so a caller that held it while it
    // waited would keep the others from ever asking.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let shared = Shared::default();
    let walks = Arc::new(AtomicU64::new(0));
    let entered = Arc::new(AtomicU64::new(0));
    let (release, wait) = mpsc::channel::<()>();
    let wait = Arc::new(std::sync::Mutex::new(wait));
    let answers = rt.block_on(async {
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let (shared, walks, entered, wait) = (
                    shared.clone(),
                    Arc::clone(&walks),
                    Arc::clone(&entered),
                    Arc::clone(&wait),
                );
                tokio::spawn(async move {
                    entered.fetch_add(1, Ordering::SeqCst);
                    let walk = move || {
                        walks.fetch_add(1, Ordering::SeqCst);
                        // Blocks until the test drops the sender.
                        assert!(wait.lock().unwrap().recv().is_err());
                        Ok(Disk {
                            today: 7,
                            ..Disk::default()
                        })
                    };
                    shared.get(day("2026-11-24"), walk).await
                })
            })
            .collect();
        while entered.load(Ordering::SeqCst) < 8 || walks.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
        drop(release);
        let mut answers = Vec::new();
        for caller in callers {
            answers.push(caller.await.unwrap().unwrap().today);
        }
        answers
    });
    assert_eq!(answers, [7; 8]);
    assert_eq!(walks.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_caller_that_gives_up_leaves_its_walk_to_the_next() {
    let shared = Shared::default();
    let walks = Arc::new(AtomicU64::new(0));
    let (release, wait) = mpsc::channel::<()>();
    let today = day("2026-11-24");
    let counted = Arc::clone(&walks);
    let first = tokio::spawn({
        let shared = shared.clone();
        async move {
            shared
                .get(today, move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    assert!(wait.recv().is_err());
                    Ok(Disk {
                        today: 5,
                        ..Disk::default()
                    })
                })
                .await
        }
    });
    while walks.load(Ordering::SeqCst) < 1 {
        tokio::task::yield_now().await;
    }
    first.abort();
    drop(release);
    let next = shared.get(today, walk(&walks, 9)).await;
    assert_eq!(next.unwrap().today, 5);
    assert_eq!(walks.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_caller_stops_waiting_for_a_hung_walk_and_the_next_gets_its_result() {
    let shared = Shared::default();
    let walks = Arc::new(AtomicU64::new(0));
    let (release, wait) = mpsc::channel::<()>();
    let today = day("2026-11-24");
    let counted = Arc::clone(&walks);
    let first = tokio::spawn({
        let shared = shared.clone();
        async move {
            shared
                .get(today, move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    assert!(wait.recv().is_err());
                    Ok(Disk {
                        today: 5,
                        ..Disk::default()
                    })
                })
                .await
        }
    });
    while walks.load(Ordering::SeqCst) < 1 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(MAX_WAIT + Duration::from_secs(1)).await;
    for _ in 0..1000 {
        if first.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    // A caller that kept waiting would hold the test forever.
    let gave_up = first.is_finished();
    drop(release);
    assert!(gave_up, "the caller still waits for the walk");
    assert_eq!(
        first.await.unwrap(),
        Err("the disk walk is still running after 30 s, ask again later".to_owned())
    );
    while shared.0.try_lock().map_or(true, |last| last.is_none()) {
        tokio::task::yield_now().await;
    }
    let next = shared.get(today, walk(&walks, 9)).await;
    assert_eq!(next.unwrap().today, 5);
    assert_eq!(walks.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_walk_slower_than_a_minute_answers_for_the_minute_after_it_ends() {
    let shared = Shared::default();
    let walks = Arc::new(AtomicU64::new(0));
    let (release, wait) = mpsc::channel::<()>();
    let today = day("2026-11-24");
    let counted = Arc::clone(&walks);
    let first = tokio::spawn({
        let shared = shared.clone();
        async move {
            shared
                .get(today, move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    assert!(wait.recv().is_err());
                    Ok(Disk {
                        today: 5,
                        ..Disk::default()
                    })
                })
                .await
        }
    });
    while walks.load(Ordering::SeqCst) < 1 {
        tokio::task::yield_now().await;
    }
    // The walk hangs for 70 s, as on a slow network mount; its caller
    // stops waiting at 30 s.
    tokio::time::advance(MAX_AGE + Duration::from_secs(10)).await;
    let gave_up = first.await.unwrap();
    drop(release);
    while shared.0.try_lock().map_or(true, |last| last.is_none()) {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(5)).await;
    let next = shared.get(today, walk(&walks, 9)).await;
    assert_eq!(
        (gave_up, next.map(|d| d.today)),
        (
            Err("the disk walk is still running after 30 s, ask again later".to_owned()),
            Ok(5)
        )
    );
    assert_eq!(walks.load(Ordering::SeqCst), 1);
}
