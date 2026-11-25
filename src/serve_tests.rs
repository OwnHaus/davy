use super::*;
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
