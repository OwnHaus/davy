//! Runs HTTP checks for monitors that are due and stores each result
//! through the writer.

use super::{CheckUrl, Expect, Outcome, Reason, store};
use crate::{
    net,
    store::{Job, Write, queue_and_wait},
    tasks, time,
};
use rusqlite::Connection;
use std::{sync::mpsc, time::Duration, time::Instant};

const MAX_CHECKS: usize = 16;
const MAX_REDIRECTS: usize = 5;
const MAX_BODY: usize = 1024 * 1024;

/// An HTTP monitor that is due for a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Http {
    pub id: i64,
    pub url: CheckUrl,
    pub expect: Expect,
    pub timeout: Duration,
}

/// The result of one check. `ms` is the time until the response headers
/// arrived, or until the check failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub outcome: Outcome,
    pub status: Option<u16>,
    pub ms: i64,
}

/// The client every check uses.
pub fn client(reach: net::Reach) -> reqwest::Result<net::Client> {
    net::Client::new(reach, MAX_REDIRECTS)
}

/// Sends `GET` to the monitor's URL and judges the answer.
pub async fn check(client: &net::Client, http: &Http) -> Checked {
    let start = Instant::now();
    let elapsed = || i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
    let failed = |e: reqwest::Error, status| Checked {
        outcome: Outcome::Down(if net::not_public(&e) {
            Reason::NotPublic
        } else if e.is_timeout() {
            Reason::Timeout
        } else {
            Reason::Connect(net::error_text(e))
        }),
        status,
        ms: elapsed(),
    };
    let Ok(request) = client.request(reqwest::Method::GET, http.url.as_str()) else {
        return Checked {
            outcome: Outcome::Down(Reason::NotPublic),
            status: None,
            ms: elapsed(),
        };
    };
    let sent = request.timeout(http.timeout).send().await;
    let mut response = match sent {
        Ok(response) => response,
        Err(e) => return failed(e, None),
    };
    let ms = elapsed();
    let status = response.status().as_u16();
    let judged = |outcome| Checked {
        outcome,
        status: Some(status),
        ms,
    };
    if status >= 400 {
        return judged(Outcome::Down(Reason::Status(status)));
    }
    let (keyword, wanted) = match &http.expect {
        Expect::Status => return judged(Outcome::Up),
        Expect::Keyword(k) => (k, true),
        Expect::NoKeyword(k) => (k, false),
    };
    let mut body = Vec::new();
    while body.len() < MAX_BODY {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(e) => return failed(e, Some(status)),
        }
    }
    body.truncate(MAX_BODY);
    let found = String::from_utf8_lossy(&body).contains(keyword.as_str());
    judged(match (found, wanted) {
        (true, true) | (false, false) => Outcome::Up,
        (false, true) => Outcome::Down(Reason::KeywordMissing),
        (true, false) => Outcome::Down(Reason::KeywordFound),
    })
}

/// Every `period`, checks the due monitors from `c`, at most 16 at a time,
/// and stores each result through `writer`. Runs until aborted.
pub async fn run(
    c: Connection,
    client: net::Client,
    writer: mpsc::SyncSender<Write>,
    period: Duration,
) {
    tasks::poll(
        "checker",
        period,
        MAX_CHECKS,
        move || store::due(&c, time::now_ms()),
        |http| http.id,
        |http| check_and_store(client.clone(), http, writer.clone()),
    )
    .await;
}

async fn check_and_store(client: net::Client, http: Http, writer: mpsc::SyncSender<Write>) {
    let at = time::now_ms();
    let checked = check(&client, &http).await;
    let job = Job::Check {
        monitor: http.id,
        at,
        checked,
    };
    queue_and_wait(&writer, job).await;
}
