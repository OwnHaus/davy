use super::*;
use crate::retention::HotDays;
use std::collections::HashMap;

fn dir() -> PathBuf {
    let data = crate::test_support::scratch("serve");
    std::fs::create_dir_all(&data).unwrap();
    data
}

fn args(data: &Path, listen: Option<&str>, public_url: Option<&str>) -> Args {
    Args {
        data: data.to_owned(),
        listen: listen.map(str::to_owned),
        public_url: public_url.map(|u| Url::parse(u).unwrap()),
        log_retention_days: 30,
        error_retention_days: 36_500,
        hot_days: HotDays::default(),
        mail_from: None,
        no_self_report: false,
    }
}

#[test]
fn missing_flags_come_from_the_settings_setup_wrote() {
    let data = dir();
    let flags = args(
        &data,
        Some("127.0.0.1:9000"),
        Some("https://flags.example/"),
    );
    assert_eq!(
        resolve(&flags).unwrap(),
        Resolved {
            listen: "127.0.0.1:9000".to_owned(),
            public_url: Url::parse("https://flags.example/").unwrap(),
            mail_from: None,
        }
    );
    assert_eq!(
        format!("{:#}", resolve(&args(&data, None, None)).unwrap_err()),
        format!(
            "cannot read {}/setup.json, run `davy setup --data {} --public-url URL` first: No such file or directory (os error 2)",
            data.display(),
            data.display()
        )
    );
    std::fs::write(
        data.join("setup.json"),
        r#"{"public_url":"https://saved.example/","listen":"127.0.0.1:8000","mail_from":"alerts@saved.example"}"#,
    )
    .unwrap();
    assert_eq!(
        resolve(&args(&data, None, Some("https://flags.example/"))).unwrap(),
        Resolved {
            listen: "127.0.0.1:8000".to_owned(),
            public_url: Url::parse("https://flags.example/").unwrap(),
            mail_from: Some("alerts@saved.example".to_owned()),
        }
    );
}

#[test]
fn the_secret_files_default_to_the_data_directory_unless_a_variable_is_set() {
    let data = dir();
    let vars = |pairs: &[(&str, &str)]| {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k: &str| map.get(k).cloned()
    };
    let token = data.join("token").to_string_lossy().into_owned();
    assert_eq!(env(&data, vars(&[]))("DAVY_TOKEN_FILE"), None);
    std::fs::write(data.join("token"), "t\n").unwrap();
    std::fs::write(data.join("smtp-url"), "smtp://x\n").unwrap();
    let defaults = env(&data, vars(&[]));
    assert_eq!(defaults("DAVY_TOKEN_FILE"), Some(token));
    assert_eq!(
        defaults("DAVY_SMTP_URL_FILE"),
        Some(data.join("smtp-url").to_string_lossy().into_owned())
    );
    assert_eq!(defaults("DAVY_TOKEN"), None);
    let set = env(
        &data,
        vars(&[("DAVY_TOKEN", "x"), ("DAVY_SMTP_URL_FILE", "/run/s")]),
    );
    assert_eq!(set("DAVY_TOKEN_FILE"), None);
    assert_eq!(set("DAVY_SMTP_URL_FILE").as_deref(), Some("/run/s"));
}

/// Set in the child process of
/// [`no_self_report_installs_no_panic_hook`] to its data directory.
const CHILD_DATA: &str = "DAVY_TEST_SERVE_DATA";
/// Set in the child to `1` for `--no-self-report`.
const CHILD_NO_SELF_REPORT: &str = "DAVY_TEST_SERVE_NO_SELF_REPORT";
const THIS_TEST: &str = "serve::tests::no_self_report_installs_no_panic_hook";

/// A panic hook is the process's for good, so each case runs `davy serve`
/// in a process of its own: this test binary run again for this test. The
/// child's `run` stops at the missing setup, after the hook would be set,
/// and the child panics. With `--no-self-report` the panic is printed and
/// no crash file is saved; without it, the hook saves one.
#[test]
fn no_self_report_installs_no_panic_hook() {
    if let Some(data) = std::env::var_os(CHILD_DATA) {
        let mut flags = args(Path::new(&data), None, None);
        flags.no_self_report = std::env::var_os(CHILD_NO_SELF_REPORT).is_some();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let stopped = runtime.block_on(run(flags)).unwrap_err();
        assert!(
            format!("{stopped:#}").starts_with("cannot read "),
            "{stopped:#}"
        );
        panic!("a panic of the child of {THIS_TEST}");
    }
    let saved = |no_self_report: bool| {
        let data = dir();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", THIS_TEST, "--test-threads=1", "--nocapture"])
            .env(CHILD_DATA, &data)
            .env_remove(CHILD_NO_SELF_REPORT);
        if no_self_report {
            child.env(CHILD_NO_SELF_REPORT, "1");
        }
        let out = child.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !out.status.success() && stderr.contains("a panic of the child of"),
            "{stderr}"
        );
        let files = std::fs::read_dir(crate::self_report::crash::dir(&data));
        files.map_or(0, Iterator::count)
    };
    assert_eq!((saved(true), saved(false)), (0, 1));
}

/// `davy serve` on `data`, listening on an address another socket holds,
/// so a server that gets past its checks fails at once instead of serving.
fn serve_on(data: &Path, flags: bool) -> String {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let listen = taken.local_addr().unwrap().to_string();
    let mut a = if flags {
        args(data, Some(&listen), Some("https://davy.example/"))
    } else {
        args(data, None, None)
    };
    a.no_self_report = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let e = rt.block_on(run(a)).unwrap_err();
    format!("{e:#}")
}

#[test]
fn a_server_does_not_start_while_a_restore_runs() {
    let data = dir();
    let _restore = crate::backup::swap::Claim::restore(&data).unwrap();
    assert_eq!(
        serve_on(&data, true),
        format!(
            "a restore into {} runs: start the server when it ends",
            data.display()
        )
    );
}

/// A restore stopped right after its commit: the old settings and token are
/// still in place, and the restored ones wait in the journal. The server
/// must read the restored ones, here a token too short to start with.
#[test]
fn a_server_finishes_a_stopped_restore_before_it_reads_its_settings() {
    let data = dir();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let setup = format!(
        r#"{{"public_url":"https://davy.example/","listen":"{}"}}"#,
        taken.local_addr().unwrap()
    );
    std::fs::write(data.join("setup.json"), &setup).unwrap();
    std::fs::write(data.join(TOKEN_FILE), "o".repeat(40)).unwrap();
    let ready = data.join(".restore-ready");
    std::fs::create_dir(&ready).unwrap();
    std::fs::write(ready.join("setup.json"), &setup).unwrap();
    std::fs::write(ready.join(TOKEN_FILE), "short").unwrap();
    std::fs::write(ready.join(".aside"), ".replaced-T-1").unwrap();
    assert_eq!(
        serve_on(&data, false),
        "the admin token has 5 characters and needs 32, `davy setup` writes one to DATA/token"
    );
    assert_eq!(
        std::fs::read_to_string(data.join(TOKEN_FILE)).unwrap(),
        "short"
    );
    assert!(!ready.exists());
}
