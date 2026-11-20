//! Sends queued notifications over HTTP. The rows are read here and marked
//! through the writer, like every other write in the server.

use super::{
    Channel,
    store::{self, Due},
};
use crate::{
    event::Timestamp,
    net,
    store::{Job, Write},
    tasks, time,
};
use rusqlite::Connection;
use std::{sync::mpsc, time::Duration};

const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// The client every delivery uses. A redirect is a failed send, so a
/// receiver cannot bounce a signed body to another address.
pub fn client(reach: net::Reach) -> reqwest::Result<net::Client> {
    net::Client::new(reach, 0)
}

/// Sends one notification at `now`. Any status but `2xx` is an error. An
/// error never holds the URL, because the URL is a credential.
pub async fn send(client: &net::Client, due: &Due, now: Timestamp) -> Result<(), String> {
    let (url, signed) = match &due.to {
        Channel::Webhook { url, secret } => {
            let signature = secret
                .sign(now.div_euclid(1000), &due.payload)
                .map_err(|e| e.to_string())?;
            (url, Some(signature))
        }
        Channel::Slack { url } => (url, None),
    };
    let mut request = client
        .request(reqwest::Method::POST, url)
        .map_err(|e| e.to_string())?;
    if let Some(signature) = signed {
        request = request
            .header("davy-delivery", due.id)
            .header("davy-signature", signature);
    }
    let response = request
        .timeout(SEND_TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(due.payload.clone())
        .send()
        .await
        .map_err(net::error_text)?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", status.as_u16()))
    }
}

/// Every `period`, sends the due rows from `c`, one at a time per channel,
/// and marks each through `writer`. A channel stays busy until its result is
/// stored, so a row is not sent twice by one server. Runs until aborted.
pub async fn run(
    c: Connection,
    client: net::Client,
    writer: mpsc::SyncSender<Write>,
    period: Duration,
) {
    tasks::poll(
        "notifier",
        period,
        usize::MAX,
        move || store::due(&c, time::now_ms()),
        |due| due.channel,
        |due| deliver(client.clone(), due, writer.clone()),
    )
    .await;
}

async fn deliver(client: net::Client, due: Due, writer: mpsc::SyncSender<Write>) {
    let result = send(&client, &due, time::now_ms()).await;
    let job = Job::Delivery {
        id: due.id,
        at: time::now_ms(),
        result,
    };
    crate::store::queue_and_wait(&writer, job).await;
}
