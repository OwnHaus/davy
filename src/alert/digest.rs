//! Issue notices that wait together and go out as one message.

use super::{Issue, Kind, Mail, Notice, notice::slack};
use crate::{event::Timestamp, text, time};
use serde::{Deserialize, Serialize};
use url::Url;

/// How long the first issue notice of a channel waits for more. A broken
/// deploy or a batch of test events makes its issues within seconds, and a
/// new issue is not urgent enough to page in the first 30 s.
pub const WINDOW_MS: i64 = 30_000;
/// Slack and e-mail list this many issues, then link to the rest. With
/// titles cut to 200 characters a Slack text stays far below the 40,000
/// characters after which Slack cuts it.
const LISTED: usize = 5;

/// The `issue` column of a notification row: what a digest needs to show
/// the issue when the row is not sent alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub project: String,
    pub issue: Issue,
}

/// One issue notice of a digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub regressed: bool,
    pub project: String,
    pub issue: Issue,
}

impl Entry {
    fn notice(&self) -> Notice {
        if self.regressed {
            Notice::IssueRegressed(self.issue.clone())
        } else {
            Notice::IssueNew(self.issue.clone())
        }
    }

    /// The issue's page in the web UI.
    fn link(&self, base: &str) -> String {
        format!("{base}/ui/issues/{}", self.issue.id)
    }
}

/// Two or more issue notices of one channel, oldest first.
#[derive(Debug, PartialEq, Eq)]
pub struct Digest {
    /// The oldest row, whose ID and time the digest carries, so a repeat
    /// after a failed send has the same ID.
    pub id: i64,
    pub at: Timestamp,
    pub entries: Vec<Entry>,
}

#[derive(Serialize)]
struct Webhook<'a> {
    id: i64,
    #[serde(rename = "type")]
    kind: &'static str,
    project: Option<&'a str>,
    at: String,
    issues: Vec<WebhookIssue<'a>>,
}

#[derive(Serialize)]
struct WebhookIssue<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    project: &'a str,
    url: String,
    issue: &'a Issue,
}

impl Digest {
    /// The body sent to a channel of `kind`. A webhook gets every issue,
    /// because a receiver may act on each one. Links start at `public_url`.
    pub fn render(&self, kind: Kind, public_url: &Url) -> serde_json::Result<String> {
        let base = public_url.as_str().trim_end_matches('/');
        match kind {
            Kind::Webhook => serde_json::to_string(&self.webhook(base)),
            Kind::Slack => serde_json::to_string(&serde_json::json!({
                "text": slack(&self.text(base)),
                "unfurl_links": false,
                "unfurl_media": false,
            })),
            Kind::Email => serde_json::to_string(&Mail {
                subject: text::printable(&self.line(), ' ').into_owned(),
                text: format!("{}\n", self.text(base)),
                message_id: format!("{}.{}", self.id, self.at),
            }),
        }
    }

    fn webhook<'a>(&'a self, base: &str) -> Webhook<'a> {
        let issues = self
            .entries
            .iter()
            .map(|entry| WebhookIssue {
                kind: entry.notice().kind(),
                project: &entry.project,
                url: entry.link(base),
                issue: &entry.issue,
            })
            .collect();
        Webhook {
            id: self.id,
            kind: "issue.digest",
            project: self.project(),
            at: time::rfc3339_ms(self.at),
            issues,
        }
    }

    /// The line, then each listed issue with its link, then a link to the
    /// rest.
    fn text(&self, base: &str) -> String {
        let one = self.project().is_some();
        let mut parts = vec![self.line()];
        for entry in self.entries.iter().take(LISTED) {
            let line = entry.notice().line((!one).then_some(&entry.project));
            parts.push(format!("{line}\n{}", entry.link(base)));
        }
        if let Some(more) = self.entries.len().checked_sub(LISTED).filter(|&n| n > 0) {
            parts.push(format!("and {more} more: {base}/ui/issues"));
        }
        parts.join("\n\n")
    }

    /// One line such as `12 new issues in 5 projects` or
    /// `[shop] 2 new and 1 regressed issues`.
    fn line(&self) -> String {
        let regressed = self.entries.iter().filter(|e| e.regressed).count();
        let new = self.entries.len() - regressed;
        let what = match (new, regressed) {
            (new, 0) => format!("{new} new issues"),
            (0, regressed) => format!("{regressed} regressed issues"),
            (new, regressed) => format!("{new} new and {regressed} regressed issues"),
        };
        match self.project() {
            Some(project) => format!("[{project}] {what}"),
            None => format!("{what} in {} projects", self.projects()),
        }
    }

    /// The project of every notice, when they share one.
    fn project(&self) -> Option<&str> {
        let first = &self.entries.first()?.project;
        self.entries
            .iter()
            .all(|e| &e.project == first)
            .then_some(first.as_str())
    }

    fn projects(&self) -> usize {
        let mut seen: Vec<&str> = self.entries.iter().map(|e| e.project.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    }
}
