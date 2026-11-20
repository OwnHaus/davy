use super::*;

fn settings(mail_from: Option<&str>) -> Settings {
    Settings {
        public_url: "https://davy.example.com/".to_owned(),
        listen: "127.0.0.1:8000".to_owned(),
        mail_from: mail_from.map(str::to_owned),
    }
}

fn serve(exe: &str, data: &str, mail_from: Option<&str>) -> Serve {
    Serve::new(
        Path::new(exe),
        Path::new(data),
        &settings(mail_from),
        mail_from.is_some(),
    )
    .unwrap()
}

#[test]
fn the_systemd_unit_runs_as_the_user_and_writes_only_the_data_directory() {
    let unit = serve("/usr/local/bin/davy", "/var/lib/davy", None).systemd("davy");
    assert_eq!(
        unit,
        "[Unit]
Description=Davy error tracking, logs and uptime
Documentation=https://github.com/dominikzabcik/davy
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
User=davy
ExecStart=/usr/local/bin/davy serve --data /var/lib/davy --listen 127.0.0.1:8000 --public-url https://davy.example.com/
Environment=DAVY_TOKEN_FILE=/var/lib/davy/token
Restart=on-failure
RestartSec=2
UMask=0077
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/davy
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
CapabilityBoundingSet=

[Install]
WantedBy=multi-user.target
"
    );
}

#[test]
fn the_systemd_unit_quotes_spaces_and_escapes_specifiers() {
    let unit = serve("/home/ops/bin/davy", "/srv/my data", Some("Davy <a@x.io>")).systemd("ops");
    let lines: Vec<&str> = unit
        .lines()
        .filter(|l| {
            l.starts_with("ExecStart=")
                || l.starts_with("Environment=")
                || l.starts_with("ProtectHome=")
                || l.starts_with("ReadWritePaths=")
        })
        .collect();
    assert_eq!(
        lines,
        [
            "ExecStart=/home/ops/bin/davy serve --data \"/srv/my data\" --listen 127.0.0.1:8000 --public-url https://davy.example.com/ --mail-from \"Davy <a@x.io>\"",
            "Environment=\"DAVY_TOKEN_FILE=/srv/my data/token\"",
            "Environment=\"DAVY_SMTP_URL_FILE=/srv/my data/smtp-url\"",
            "ProtectHome=read-only",
            "ReadWritePaths=\"/srv/my data\"",
        ]
    );
    assert_eq!(unit_word("50%$x"), "50%%$$x");
    assert_eq!(unit_word(r#"a"b\c"#), r#""a\"b\\c""#);
}

#[test]
fn a_line_break_in_any_word_is_refused() {
    let err = Serve::new(
        Path::new("/usr/bin/davy"),
        Path::new("/var/lib/davy"),
        &settings(Some("a@x.io\nExecStartPre=/bin/sh")),
        true,
    )
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "\"a@x.io\\nExecStartPre=/bin/sh\" has a control character, choose a path or value without one"
    );
}

#[test]
fn the_launchd_job_escapes_xml_and_logs_to_the_data_directory() {
    let plist = serve("/opt/davy", "/Users/me/davy", Some("Davy <a@x.io>")).launchd();
    assert_eq!(
        plist,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>davy</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/davy</string>
    <string>serve</string>
    <string>--data</string>
    <string>/Users/me/davy</string>
    <string>--listen</string>
    <string>127.0.0.1:8000</string>
    <string>--public-url</string>
    <string>https://davy.example.com/</string>
    <string>--mail-from</string>
    <string>Davy &lt;a@x.io&gt;</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>DAVY_TOKEN_FILE</key><string>/Users/me/davy/token</string>
    <key>DAVY_SMTP_URL_FILE</key><string>/Users/me/davy/smtp-url</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/Users/me/davy/davy.log</string>
  <key>StandardErrorPath</key><string>/Users/me/davy/davy.log</string>
</dict>
</plist>
"#
    );
}

#[test]
fn the_shell_line_names_the_secret_files_and_quotes_words() {
    assert_eq!(
        serve("/usr/local/bin/davy", "/srv/my data", Some("Davy <a@x.io>")).shell(),
        "DAVY_TOKEN_FILE='/srv/my data/token' DAVY_SMTP_URL_FILE='/srv/my data/smtp-url' /usr/local/bin/davy serve --data '/srv/my data' --listen 127.0.0.1:8000 --public-url https://davy.example.com/ --mail-from 'Davy <a@x.io>'"
    );
}
