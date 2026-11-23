//! What a notice says, and the body each channel kind gets for it.

use super::Kind;
use crate::{event::Timestamp, text, time};
use serde::{Deserialize, Serialize};

/// Slack shows a long title in full, so one event cannot fill a channel.
const MAX_TEXT_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub id: i64,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub release: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incident {
    pub id: i64,
    pub monitor: i64,
    /// The monitor's name.
    pub name: String,
    pub reason: String,
}

/// A change a team hears about. It holds what the body shows, read in the
/// same transaction as the change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    IssueNew(Issue),
    IssueRegressed(Issue),
    MonitorDown(Incident),
    MonitorUp(Incident),
    /// What an agent reported with `submit_feedback`, row `id`.
    Feedback {
        id: i64,
        feedback: crate::feedback::Feedback,
    },
    Test,
}

/// The payload of an e-mail row: what the notifier puts in the message.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    pub subject: String,
    pub text: String,
    /// `<row id>.<created ms>`, the local part of the `Message-ID` after
    /// `davy.`, so a repeat after a restart has the same ID.
    pub message_id: String,
}

#[derive(Serialize)]
struct Webhook<'a> {
    id: i64,
    #[serde(rename = "type")]
    kind: &'static str,
    project: Option<&'a str>,
    at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue: Option<&'a Issue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monitor: Option<Monitor<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    incident: Option<IncidentBody<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    feedback: Option<FeedbackBody<'a>>,
}

#[derive(Serialize)]
struct FeedbackBody<'a> {
    id: i64,
    #[serde(flatten)]
    feedback: &'a crate::feedback::Feedback,
}

#[derive(Serialize)]
struct Monitor<'a> {
    id: i64,
    name: &'a str,
}

#[derive(Serialize)]
struct IncidentBody<'a> {
    id: i64,
    reason: &'a str,
}

impl Notice {
    /// The `type` field of a webhook body and the `type` column of its row.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::IssueNew(_) => "issue.new",
            Self::IssueRegressed(_) => "issue.regressed",
            Self::MonitorDown(_) => "monitor.down",
            Self::MonitorUp(_) => "monitor.up",
            Self::Feedback { .. } => "feedback",
            Self::Test => "test",
        }
    }

    /// The JSON body sent to a channel of `kind` for notification row `id`.
    /// `project` is `None` for a test notice to a channel of every project.
    pub fn render(
        &self,
        kind: Kind,
        id: i64,
        project: Option<&str>,
        at: Timestamp,
    ) -> serde_json::Result<String> {
        match kind {
            Kind::Webhook => serde_json::to_string(&self.webhook(id, project, at)),
            Kind::Slack => serde_json::to_string(&serde_json::json!({
                "text": slack(&self.line(project)),
                "unfurl_links": false,
                "unfurl_media": false,
            })),
            Kind::Email => serde_json::to_string(&self.mail(id, project, at)),
        }
    }

    fn mail(&self, id: i64, project: Option<&str>, at: Timestamp) -> Mail {
        let line = self.line(project);
        let details = self
            .details(at)
            .into_iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!("{line}\n\n{details}\n");
        Mail {
            subject: text::printable(&line, ' ').into_owned(),
            text,
            message_id: format!("{id}.{at}"),
        }
    }

    /// The `name: value` lines of an e-mail body.
    fn details(&self, at: Timestamp) -> Vec<(&'static str, String)> {
        let mut lines = match self {
            Self::IssueNew(issue) | Self::IssueRegressed(issue) => {
                let mut lines = vec![
                    ("title", cut(&issue.title)),
                    ("issue_id", issue.id.to_string()),
                    ("level", issue.level.clone()),
                ];
                lines.extend(issue.culprit.as_deref().map(|c| ("culprit", cut(c))));
                lines.extend(issue.release.as_deref().map(|r| ("release", cut(r))));
                lines
            }
            Self::MonitorDown(i) | Self::MonitorUp(i) => vec![
                ("monitor", i.name.clone()),
                ("monitor_id", i.monitor.to_string()),
                ("incident_id", i.id.to_string()),
                ("reason", cut(&i.reason)),
            ],
            Self::Feedback { id, feedback } => {
                let mut lines = vec![
                    ("feedback_id", id.to_string()),
                    ("client", client(feedback).to_owned()),
                    ("tried", feedback.tried.clone()),
                    ("happened", feedback.happened.clone()),
                ];
                lines.extend(feedback.fix.clone().map(|f| ("fix", f)));
                lines
            }
            Self::Test => Vec::new(),
        };
        lines.push(("at", time::rfc3339_ms(at)));
        lines
    }

    fn webhook<'a>(&'a self, id: i64, project: Option<&'a str>, at: Timestamp) -> Webhook<'a> {
        let (issue, incident) = match self {
            Self::IssueNew(issue) | Self::IssueRegressed(issue) => (Some(issue), None),
            Self::MonitorDown(incident) | Self::MonitorUp(incident) => (None, Some(incident)),
            Self::Feedback { .. } | Self::Test => (None, None),
        };
        let feedback = match self {
            Self::Feedback { id, feedback } => Some(FeedbackBody { id: *id, feedback }),
            _ => None,
        };
        Webhook {
            id,
            kind: self.kind(),
            project,
            at: time::rfc3339_ms(at),
            issue,
            monitor: incident.map(|i| Monitor {
                id: i.monitor,
                name: &i.name,
            }),
            incident: incident.map(|i| IncidentBody {
                id: i.id,
                reason: &i.reason,
            }),
            feedback,
        }
    }

    /// The issue of an issue notice.
    #[must_use]
    pub fn issue(&self) -> Option<&Issue> {
        match self {
            Self::IssueNew(issue) | Self::IssueRegressed(issue) => Some(issue),
            _ => None,
        }
    }

    /// One line such as `[shop] API is down: HTTP 503`.
    pub(super) fn line(&self, project: Option<&str>) -> String {
        let text = match self {
            Self::IssueNew(issue) => format!("New issue: {}", cut(&issue.title)),
            Self::IssueRegressed(issue) => format!("Issue regressed: {}", cut(&issue.title)),
            Self::MonitorDown(i) => format!("{} is down: {}", i.name, cut(&i.reason)),
            Self::MonitorUp(i) => format!("{} is up again", i.name),
            Self::Feedback { feedback, .. } => format!(
                "Feedback from {}: {}",
                client(feedback),
                cut(&feedback.happened)
            ),
            Self::Test => "Test notice from Davy".to_owned(),
        };
        match project {
            Some(project) => format!("[{project}] {text}"),
            None => text,
        }
    }
}

fn client(feedback: &crate::feedback::Feedback) -> &str {
    feedback.client.as_deref().unwrap_or("an agent")
}

fn cut(s: &str) -> String {
    s.chars().take(MAX_TEXT_CHARS).collect()
}

/// Escaped so a title from an event cannot mention `@channel` or hide a link.
pub(super) fn slack(line: &str) -> String {
    line.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
