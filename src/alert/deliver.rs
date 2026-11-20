//! Sends queued notifications over HTTP. The rows are read here and marked
//! through the writer, like every other write in the server.

use super::{
    Channel,
    store::{self, Due},
};
use crate::{
    event::Timestamp,
    store::{Job, Write},
    time,
};
use rusqlite::Connection;
use std::{collections::HashMap, sync::mpsc, time::Duration};
use tokio::{sync::oneshot, task::JoinSet};

const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const QUEUE_FULL_WAIT: Duration = Duration::from_millis(50);

/// The client every delivery uses. A redirect is a failed send, so a
/// receiver cannot bounce a signed body to another address.
pub fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Davy/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Sends one notification at `now`. Any status but `2xx` is an error. An
/// error never holds the URL, because the URL is a credential.
pub async fn send(client: &reqwest::Client, due: &Due, now: Timestamp) -> Result<(), String> {
    let request = match &due.to {
        Channel::Webhook { url, secret } => {
            let signature = secret
                .sign(now.div_euclid(1000), &due.payload)
                .map_err(|e| e.to_string())?;
            client
                .post(url)
                .header("davy-delivery", due.id)
                .header("davy-signature", signature)
        }
        Channel::Slack { url } => client.post(url),
    };
    let response = request
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(due.payload.clone())
        .send()
        .await
        .map_err(|e| causes(&e.without_url()))?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", status.as_u16()))
    }
}

/// An error with its causes, which say why a connection failed.
fn causes(e: &(dyn std::error::Error + 'static)) -> String {
    std::iter::successors(Some(e), |e| e.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// Every `period`, sends the due rows from `c`, one at a time per channel,
/// and marks each through `writer`. A channel stays busy until its result is
/// stored, so a row is not sent twice by one server. Runs until aborted.
pub async fn run(
    c: Connection,
    client: reqwest::Client,
    writer: mpsc::SyncSender<Write>,
    period: Duration,
) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut busy: HashMap<tokio::task::Id, i64> = HashMap::new();
    let mut sends = JoinSet::new();
    loop {
        tokio::select! {
            _ = ticks.tick() => {
                let due = match store::due(&c, time::now_ms()) {
                    Ok(due) => due,
                    Err(e) => {
                        report(&e);
                        continue;
                    }
                };
                for due in due {
                    if busy.values().any(|&channel| channel == due.channel) {
                        continue;
                    }
                    let channel = due.channel;
                    let task = sends.spawn(deliver(client.clone(), due, writer.clone()));
                    busy.insert(task.id(), channel);
                }
            }
            Some(done) = sends.join_next_with_id() => {
                let id = match done {
                    Ok((id, ())) => id,
                    Err(e) => e.id(),
                };
                busy.remove(&id);
            }
        }
    }
}

/// A failed read is tried again on the next tick. The error holds no URL.
#[expect(
    clippy::print_stderr,
    reason = "the server has no logger yet, stderr is its log"
)]
fn report(e: &anyhow::Error) {
    eprintln!("notifier: reading due notifications: {e:#}");
}

async fn deliver(client: reqwest::Client, due: Due, writer: mpsc::SyncSender<Write>) {
    let result = send(&client, &due, time::now_ms()).await;
    let mut job = Job::Delivery {
        id: due.id,
        at: time::now_ms(),
        result,
    };
    loop {
        let (done, stored) = oneshot::channel();
        match writer.try_send(Write { job, done }) {
            Ok(()) => {
                drop(stored.await);
                return;
            }
            Err(mpsc::TrySendError::Full(write)) => {
                job = write.job;
                tokio::time::sleep(QUEUE_FULL_WAIT).await;
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return,
        }
    }
}
