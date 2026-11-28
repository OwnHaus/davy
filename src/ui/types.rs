//! The TypeScript types of the UI API in `ui/src/api/types.ts`, made from
//! the Rust types, so a change on one side fails the tests until the other
//! side follows.

use super::{
    account::{LoggedIn, Login, Me, NewPassword, Welcome},
    agents::{LedgerPage, TokenChange},
    alerts::{Delivery, Notification, NotificationPage},
    connect::{Endpoints, ProjectKey, Setup, Step},
    issues::{IssueView, LogLine, Logs, StatusChange},
    logs::{ColdCut, LogHistogram, LogPage},
    monitors::{Check, Hour, Incident, MonitorDetail, MonitorKind, MonitorPage, MonitorRow},
};
use crate::{
    access::Level,
    alert::{Kind, store::Listed},
    event_view::{Breadcrumb, Context, Cut, EventView, ExceptionView, FrameView, Pair, Request},
    firsts::First,
    issues::{EventItem, EventPage, IssuePage, IssueRow, Project, Sort, Status},
    ledger::{ActorKind, Outcome, Row},
    logs::{LogLevel, LogSource},
    monitor::State,
    retention::{Change, Kept, Retention, ServerDefault, Setting},
    store::{IssueDetail, ReleaseCount},
    tokens,
    usage::days::{Counts, OtherDays, ProjectDays, ProjectStored, UsageDays},
};
use ts_rs::{Config, TS};

const PATH: &str = "ui/src/api/types.ts";
const HEADER: &str =
    "// Made from the Rust types by `DAVY_WRITE_TYPES=1 cargo test ui::types`.\n// Do not edit.\n";

fn decl<T: TS>(config: &Config) -> String {
    format!("\nexport {}\n", T::decl(config))
}

/// Every type the browser sends or reads, in the order of the file.
fn file() -> String {
    // Counts and ids fit in a JavaScript number, so they are not bigint.
    let c = Config::new().with_large_int("number");
    let decls = [
        decl::<Me>(&c),
        decl::<Login>(&c),
        decl::<LoggedIn>(&c),
        decl::<Welcome>(&c),
        decl::<NewPassword>(&c),
        decl::<Sort>(&c),
        decl::<Status>(&c),
        decl::<Project>(&c),
        decl::<IssueRow>(&c),
        decl::<IssuePage>(&c),
        decl::<IssueDetail>(&c),
        decl::<ReleaseCount>(&c),
        decl::<IssueView>(&c),
        decl::<EventItem>(&c),
        decl::<EventPage>(&c),
        decl::<Cut<Pair>>(&c),
        decl::<EventView>(&c),
        decl::<ExceptionView>(&c),
        decl::<FrameView>(&c),
        decl::<Breadcrumb>(&c),
        decl::<Pair>(&c),
        decl::<Request>(&c),
        decl::<Context>(&c),
        decl::<LogSource>(&c),
        decl::<LogLevel>(&c),
        decl::<serde_json::Value>(&c),
        decl::<LogLine>(&c),
        decl::<ColdCut>(&c),
        decl::<Logs>(&c),
        decl::<LogPage>(&c),
        decl::<LogHistogram>(&c),
        decl::<StatusChange>(&c),
        decl::<MonitorKind>(&c),
        decl::<State>(&c),
        decl::<Hour>(&c),
        decl::<MonitorRow>(&c),
        decl::<MonitorPage>(&c),
        decl::<Incident>(&c),
        decl::<Check>(&c),
        decl::<MonitorDetail>(&c),
        decl::<Kind>(&c),
        decl::<Listed>(&c),
        decl::<Delivery>(&c),
        decl::<Notification>(&c),
        decl::<NotificationPage>(&c),
        decl::<ProjectKey>(&c),
        decl::<Endpoints>(&c),
        decl::<First>(&c),
        decl::<Step>(&c),
        decl::<Setup>(&c),
        decl::<ServerDefault>(&c),
        decl::<Setting>(&c),
        decl::<Change>(&c),
        decl::<Kept>(&c),
        decl::<Retention>(&c),
        decl::<Counts>(&c),
        decl::<ProjectDays>(&c),
        decl::<ProjectStored>(&c),
        decl::<OtherDays>(&c),
        decl::<UsageDays>(&c),
        decl::<Level>(&c),
        decl::<tokens::Listed>(&c),
        decl::<TokenChange>(&c),
        decl::<ActorKind>(&c),
        decl::<Outcome>(&c),
        decl::<Row>(&c),
        decl::<LedgerPage>(&c),
    ];
    format!("{HEADER}{}", decls.concat())
}

#[test]
fn the_ui_types_match_the_rust_types() {
    let made = file();
    if std::env::var_os("DAVY_WRITE_TYPES").is_some() {
        std::fs::create_dir_all("ui/src/api").unwrap();
        std::fs::write(PATH, &made).unwrap();
    }
    let kept = std::fs::read_to_string(PATH).unwrap_or_default();
    assert!(
        kept == made,
        "{PATH} is not the types of the Rust code, run DAVY_WRITE_TYPES=1 cargo test ui::types"
    );
}
