//! The doctor check `token`: the admin token in DATA/token is there, long
//! enough, and readable by its owner only.

use super::{Check, Status, not_ok, ok, open_mode, sh};
use crate::secret::{self, MIN_TOKEN_CHARS};
use std::path::Path;

/// The check of the admin token file `path`, holding `token` when it could
/// be read, in the data directory `d` names for a shell.
pub(super) fn file(path: &Path, token: Option<&str>, d: &str) -> Check {
    let p = sh(&path.to_string_lossy());
    let Some(token) = token else {
        return not_ok(
            "token",
            Status::Fail,
            format!("{p} is missing"),
            format!("davy setup --data {d} --public-url URL"),
        );
    };
    let chars = secret::line(token).chars().count();
    if chars < MIN_TOKEN_CHARS {
        return not_ok(
            "token",
            Status::Fail,
            format!("{p} holds {chars} characters and needs {MIN_TOKEN_CHARS}"),
            format!(
                "rm {p} && davy setup --data {d} --public-url URL, then update every client of the old token"
            ),
        );
    }
    match open_mode(path) {
        Some(mode) => not_ok(
            "token",
            Status::Warn,
            format!("{p} can be read by other users (mode {mode:o})"),
            format!("chmod 600 {p}"),
        ),
        None => ok("token", format!("{p} holds a token of {chars} characters")),
    }
}
