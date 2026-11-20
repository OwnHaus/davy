//! Secrets the server reads at start. `NAME` holds the value itself and
//! `NAME_FILE` the path of a file that holds it, as systemd credentials and
//! container secrets hand them over. Neither ever appears on a command line.

use anyhow::{Context as _, bail, ensure};

/// The admin token is at least this long, so it cannot be guessed online.
pub const MIN_TOKEN_CHARS: usize = 32;

/// Reads the secret `name` through `env`, which is `std::env::var` outside
/// tests. An empty variable counts as unset. A file loses its trailing line
/// break.
pub fn read(name: &str, env: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<String>> {
    let file_var = format!("{name}_FILE");
    let get = |key: &str| env(key).filter(|v| !v.is_empty());
    match (get(name), get(&file_var)) {
        (Some(_), Some(_)) => bail!("set {name} or {file_var}, not both"),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(path)) => {
            let text = std::fs::read_to_string(&path).with_context(|| {
                format!("cannot read {file_var} {path:?}, check that it exists and that this user may read it")
            })?;
            let value = text.trim_end_matches(['\n', '\r']);
            ensure!(!value.is_empty(), "{file_var} {path:?} is empty");
            Ok(Some(value.to_owned()))
        }
        (None, None) => Ok(None),
    }
}

/// Reads `DAVY_TOKEN` or `DAVY_TOKEN_FILE` and refuses a short token.
pub fn admin_token(env: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<String>> {
    let token = read("DAVY_TOKEN", env)?;
    if let Some(t) = &token {
        ensure!(
            t.chars().count() >= MIN_TOKEN_CHARS,
            "the admin token has {} characters and needs {MIN_TOKEN_CHARS}, `davy setup` writes one to DATA/token",
            t.chars().count()
        );
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, path::PathBuf};

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn file(contents: &str) -> PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("test-secret-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn a_secret_comes_from_the_variable_or_the_file_without_its_line_break() {
        assert_eq!(
            read("X", env(&[("X", "abc")])).unwrap().as_deref(),
            Some("abc")
        );
        let path = file("from-file\r\n");
        let p = path.to_str().unwrap();
        assert_eq!(
            read("X", env(&[("X_FILE", p)])).unwrap().as_deref(),
            Some("from-file")
        );
        assert_eq!(read("X", env(&[("X", ""), ("X_FILE", "")])).unwrap(), None);
    }

    #[test]
    fn both_sources_an_empty_file_and_a_missing_file_are_refused() {
        let path = file("\n");
        let p = path.to_str().unwrap();
        let err = |pairs: &[(&str, &str)]| format!("{}", read("X", env(pairs)).unwrap_err());
        assert_eq!(
            err(&[("X", "a"), ("X_FILE", p)]),
            "set X or X_FILE, not both"
        );
        assert_eq!(err(&[("X_FILE", p)]), format!("X_FILE {p:?} is empty"));
        assert_eq!(
            err(&[("X_FILE", "/nonexistent/davy")]),
            "cannot read X_FILE \"/nonexistent/davy\", check that it exists and that this user may read it"
        );
    }

    #[test]
    fn the_admin_token_needs_32_characters() {
        let short = "a".repeat(31);
        let err = admin_token(env(&[("DAVY_TOKEN", &short)])).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the admin token has 31 characters and needs 32, `davy setup` writes one to DATA/token"
        );
        let ok = "a".repeat(32);
        let path = file(&format!("{ok}\n"));
        let found = admin_token(env(&[("DAVY_TOKEN_FILE", path.to_str().unwrap())])).unwrap();
        assert_eq!(found.as_deref(), Some(ok.as_str()));
    }
}
