//! The TypeScript types of the UI API in `ui/src/api/types.ts`, made from
//! the Rust types, so a change on one side fails the tests until the other
//! side follows.

use super::{
    account::{LoggedIn, Login, Me, NewPassword, Welcome},
    issues::{IssueView, LogLine, Logs, StatusChange},
};
use crate::{
    event_view::{Breadcrumb, Context, Cut, EventView, ExceptionView, FrameView, Pair, Request},
    issues::{EventItem, EventPage, IssuePage, IssueRow, Project, Sort, Status},
    logs::{LogLevel, LogSource},
    store::{IssueDetail, ReleaseCount},
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
        decl::<Logs>(&c),
        decl::<StatusChange>(&c),
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
