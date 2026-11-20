//! Sends queued notifications over HTTP or SMTP. The rows are read here and
//! marked through the writer, like every other write in the server.

use super::{
    Channel, SEND_TIMEOUT,
    mail::Smtp,
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

const NO_SMTP: &str = "no SMTP server, set DAVY_SMTP_URL";

/// What deliveries go out through.
#[derive(Clone, Debug)]
pub struct Transports {
    http: net::Client,
    smtp: Option<Smtp>,
}

impl Transports {
    /// A redirect is a failed send, so a receiver cannot bounce a signed body
    /// to another address. Without `smtp`, e-mail rows fail.
    pub fn new(reach: net::Reach, smtp: Option<Smtp>) -> reqwest::Result<Self> {
        Ok(Self {
            http: net::Client::new(reach, 0)?,
            smtp,
        })
    }
}

/// Sends one notification at `now`. Any HTTP status but `2xx` is an error.
/// An error never holds the URL, because the URL is a credential.
pub async fn send(transports: &Transports, due: &Due, now: Timestamp) -> Result<(), String> {
    let (url, signed) = match &due.to {
        Channel::Webhook { url, secret } => {
            let signature = secret
                .sign(now.div_euclid(1000), &due.payload)
                .map_err(|e| e.to_string())?;
            (url, Some(signature))
        }
        Channel::Slack { url } => (url, None),
        Channel::Email { to } => {
            let smtp = transports.smtp.as_ref().ok_or(NO_SMTP)?;
            return smtp.send(to, &due.payload).await;
        }
    };
    let mut request = transports
        .http
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
    transports: Transports,
    writer: mpsc::SyncSender<Write>,
    period: Duration,
) {
    tasks::poll(
        "notifier",
        period,
        usize::MAX,
        move || store::due(&c, time::now_ms()),
        |due| due.channel,
        |due| deliver(transports.clone(), due, writer.clone()),
    )
    .await;
}

async fn deliver(transports: Transports, due: Due, writer: mpsc::SyncSender<Write>) {
    let result = send(&transports, &due, time::now_ms()).await;
    let job = Job::Delivery {
        id: due.id,
        at: time::now_ms(),
        result,
    };
    crate::store::queue_and_wait(&writer, job).await;
}
