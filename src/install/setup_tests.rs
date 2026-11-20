use super::*;

fn dir() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize()
        .unwrap()
        .join(format!("test-setup-{}", uuid::Uuid::new_v4()))
}

fn args(data: &Path, service: Service, mail_from: Option<&str>) -> Args {
    Args {
        data: data.to_owned(),
        public_url: Url::parse("https://davy.example.com").unwrap(),
        listen: "127.0.0.1:8000".to_owned(),
        mail_from: mail_from.map(str::to_owned),
        service,
    }
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
    let data = dir();
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
                format!(
                    "DAVY_TOKEN_FILE={d}/token /usr/local/bin/davy serve --data {d} --listen 127.0.0.1:8000 --public-url https://davy.example.com/"
                ),
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
    let data = dir();
    let a = args(&data, Service::Systemd, None);
    let first = setup(&a, exe(), Some("davy")).unwrap();
    let token = std::fs::read_to_string(data.join("token")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for path in [data.clone(), data.join("token"), data.join("davy.service")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();
        }
    }
    let second = setup(&a, exe(), Some("davy")).unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read_to_string(data.join("token")).unwrap(), token);
    #[cfg(unix)]
    assert_eq!(
        [&data, &data.join("token"), &data.join("davy.service")].map(|p| mode(p)),
        [0o700, 0o600, 0o600]
    );
    let d = data.display();
    assert_eq!(second.service_file, Some(data.join("davy.service")));
    assert_eq!(
        second.next,
        [
            format!("sudo install -m 644 {d}/davy.service /etc/systemd/system/davy.service"),
            "sudo systemctl daemon-reload".to_owned(),
            "sudo systemctl enable davy".to_owned(),
            "sudo systemctl restart davy".to_owned(),
            format!("/usr/local/bin/davy doctor --data {d}"),
        ]
    );
    let unit = std::fs::read_to_string(data.join("davy.service")).unwrap();
    assert!(unit.contains(&format!("\nEnvironment=DAVY_TOKEN_FILE={d}/token\n")));
}

#[test]
fn setup_refuses_a_short_token_a_foreign_directory_and_root() {
    let data = dir();
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
    let foreign = dir();
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
    let fresh = dir();
    assert_eq!(
        err(&args(&fresh, Service::Systemd, None), Some("root")),
        "the service must not run as root. Create a user for it (docs/install.md, step 3) and run setup as that user with `runuser -u davy -- davy setup ...`"
    );
    assert_eq!(
        err(&args(&fresh, Service::Systemd, None), None),
        "USER is not set, run setup as the user the service will run as"
    );
}

#[test]
fn mail_from_and_the_smtp_file_come_together() {
    let data = dir();
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
        format!(
            "DAVY_TOKEN_FILE={d}/token DAVY_SMTP_URL_FILE={d}/smtp-url /usr/local/bin/davy serve --data {d} --listen 127.0.0.1:8000 --public-url https://davy.example.com/ --mail-from alerts@example.com"
        )
    );
}
