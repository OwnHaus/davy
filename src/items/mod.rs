//! Envelope items the SDKs send next to errors and logs, which Davy keeps in
//! SQLite: client reports, user feedback and sessions.

pub mod client_report;
pub mod health;
pub mod session;
pub mod user_feedback;

use crate::event::Timestamp;
use anyhow::Result;
use client_report::Discarded;
use rusqlite::Connection;
use session::{Aggregates, Session};
use user_feedback::UserFeedback;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdkItem {
    ClientReport(Vec<Discarded>),
    UserFeedback(UserFeedback),
    Session(Session),
    Sessions(Aggregates),
}

/// Stores the items of one envelope from `project`, received at `at`.
pub fn add(c: &Connection, project: i64, at: Timestamp, items: &[SdkItem]) -> Result<()> {
    for item in items {
        match item {
            SdkItem::ClientReport(entries) => client_report::add(c, project, at, entries)?,
            SdkItem::UserFeedback(f) => {
                if !user_feedback::add(c, project, at, f)? {
                    let over = Discarded {
                        reason: user_feedback::OVER_DAILY_CAP.to_owned(),
                        category: "feedback".to_owned(),
                        quantity: 1,
                    };
                    client_report::add(c, project, at, &[over])?;
                }
            }
            SdkItem::Session(s) => {
                if let Some(reason) = session::add_session(c, project, at, s)? {
                    let refused = Discarded {
                        reason: reason.to_owned(),
                        category: "session".to_owned(),
                        quantity: 1,
                    };
                    client_report::add(c, project, at, &[refused])?;
                }
            }
            SdkItem::Sessions(a) => {
                let refused = session::add_aggregates(c, project, at, a)?;
                client_report::add(c, project, at, &refused)?;
            }
        }
    }
    Ok(())
}
