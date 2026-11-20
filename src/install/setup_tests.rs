use super::*;
use crate::test_support::scratch;

fn args(data: &Path, service: Service, mail_from: Option<&str>) -> Args {
    Args {
        data: data.to_owned(),
        public_url: Url::parse("https://davy.example.com").unwrap(),
        listen: "127.0.0.1:8000".to_owned(),
        mail_from: mail_from.map(str::to_owned),
        service,
    }
}

/// The user running the tests, who exists. Systemd setup needs a real one.
fn me() -> String {
    nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap()
        .name
}

fn exe() -> &'static Path {
    Path::new("/usr/local/bin/davy")
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn setup_on_an_empty_directory_writes_a_private_token_and_the_settings() {
    let data = scratch("setup");
    let report = setup(&args(&data, Service::None, None), exe(), Some("davy")).unwrap();
    let d = data.display();
    assert_eq!(
        report,
        Report {
            data: data.clone(),
            token_file: data.join("token"),
            smtp: false,
            service_file: None,
            next: vec![
                format!("/usr/local/bin/davy serve --data {d}"),
                format!("/usr/local/bin/davy doctor --data {d}"),
            ],
        }
    );
    let token = std::fs::read_to_string(data.join("token")).unwrap();
    assert_eq!(token.len(), 65);
    assert!(token[..64].chars().all(|c| c.is_ascii_hexdigit()));
    assert!(token.ends_with('\n'));
    assert_eq!(
        std::fs::read_to_string(data.join("setup.json")).unwrap(),
        "{\n  \"public_url\": \"https://davy.example.com/\",\n  \"listen\": \"127.0.0.1:8000\",\n  \"mail_from\": null\n}\n"
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&data), 0o700);
        assert_eq!(mode(&data.join("token")), 0o600);
        assert_eq!(mode(&data.join("setup.json")), 0o600);
    }
}

#[test]
fn setup_twice_keeps_the_token_tightens_modes_and_prints_the_same_report() {
    let data = scratch("setup");
    let user = me();
    let a = args(&data, Service::Systemd, None);
    let first = setup(&a, exe(), Some(&user)).unwrap();
    let token = std::fs::read_to_string(data.join("token")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for path in [data.clone(), data.join("token")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();
        }
    }
    let second = setup(&a, exe(), Some(&user)).unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read_to_string(data.join("token")).unwrap(), token);
    #[cfg(unix)]
    assert_eq!(
        [&data, &data.join("token")].map(|p| mode(p)),
        [0o700, 0o600]
    );
    let d = data.display();
    assert_eq!(second.service_file, None);
    assert_eq!(
        second.next,
        [
            format!(
                "unit=\"$(/usr/local/bin/davy systemd-unit --data {d} --user {user})\" && printf '%s\\n' \"$unit\" | sudo tee /etc/systemd/system/davy.service > /dev/null"
            ),
            "sudo systemctl daemon-reload".to_owned(),
            "sudo systemctl enable davy".to_owned(),
            "sudo systemctl restart davy".to_owned(),
            format!("sudo runuser -u {user} -- /usr/local/bin/davy doctor --data {d}"),
        ]
    );
    let names: Vec<String> = std::fs::read_dir(&data)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(names, ["setup.json", "token"]);
}

#[cfg(unix)]
#[test]
fn setup_refuses_links_in_place_of_its_files() {
    let data = scratch("setup");
    std::fs::create_dir_all(&data).unwrap();
    let elsewhere = scratch("setup");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let target = elsewhere.join("known-token");
    std::fs::write(&target, "0123456789abcdef0123456789abcdef\n").unwrap();
    std::os::unix::fs::symlink(&target, data.join("token")).unwrap();
    let a = args(&data, Service::None, None);
    assert_eq!(
        format!("{:#}", setup(&a, exe(), Some("davy")).unwrap_err()),
        format!(
            "{}/token is not a regular file, replace it with one",
            data.display()
        )
    );
    std::fs::remove_file(data.join("token")).unwrap();
    let settings = elsewhere.join("victim");
    std::fs::write(&settings, "keep\n").unwrap();
    std::os::unix::fs::symlink(&settings, data.join("setup.json")).unwrap();
    setup(&a, exe(), Some("davy")).unwrap();
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "keep\n");
    assert!(
        !std::fs::symlink_metadata(data.join("setup.json"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn setup_refuses_a_short_token_a_foreign_directory_and_root() {
    let data = scratch("setup");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("token"), "short\n").unwrap();
    let err = |a: &Args, user| format!("{:#}", setup(a, exe(), user).unwrap_err());
    let a = args(&data, Service::None, None);
    assert_eq!(
        err(&a, Some("davy")),
        format!(
            "{}/token holds 5 characters and needs 32. Delete it and run setup again for a new token, then update every agent and sentry-cli that used the old one",
            data.display()
        )
    );
    let foreign = scratch("setup");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join(".profile"), "").unwrap();
    #[cfg(unix)]
    let before = mode(&foreign);
    assert_eq!(
        err(&args(&foreign, Service::None, None), Some("davy")),
        format!(
            "{} holds other files, give setup a new directory such as /var/lib/davy",
            foreign.display()
        )
    );
    #[cfg(unix)]
    assert_eq!(mode(&foreign), before);
    let fresh = scratch("setup");
    assert_eq!(
        err(&args(&fresh, Service::Systemd, None), Some("root")),
        "the service must not run as root. Create a user for it (docs/install.md, step 3) and run setup as that user with `runuser -u davy -- davy setup ...`"
    );
    let mut bad_listen = args(&fresh, Service::None, None);
    bad_listen.listen = "127.0.0.1:8000\nUser=root".to_owned();
    assert_eq!(
        err(&bad_listen, Some("davy")),
        "--listen \"127.0.0.1:8000\\nUser=root\" is not an address such as 127.0.0.1:8000: invalid socket address syntax"
    );
    assert_eq!(
        err(&args(&fresh, Service::Systemd, None), None),
        "USER is not set, run setup as the user the service will run as"
    );
}

#[test]
fn mail_from_and_the_smtp_file_come_together() {
    let data = scratch("setup");
    let with_mail = args(&data, Service::None, Some("alerts@example.com"));
    assert_eq!(
        format!("{:#}", setup(&with_mail, exe(), Some("davy")).unwrap_err()),
        format!(
            "--mail-from needs the SMTP URL in {}/smtp-url, write it there first (docs/install.md, step 5)",
            data.display()
        )
    );
    std::fs::write(data.join("smtp-url"), "smtps://u:p@mail.example.com\n").unwrap();
    assert_eq!(
        format!(
            "{:#}",
            setup(&args(&data, Service::None, None), exe(), Some("davy")).unwrap_err()
        ),
        format!(
            "{}/smtp-url exists, pass --mail-from with the sender of alert e-mail",
            data.display()
        )
    );
    let report = setup(&with_mail, exe(), Some("davy")).unwrap();
    assert!(report.smtp);
    let d = data.display();
    assert_eq!(
        report.next[0],
        format!("/usr/local/bin/davy serve --data {d}")
    );
}
