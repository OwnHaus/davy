use super::*;

fn serve(exe: &str, data: &str) -> Serve {
    Serve::new(Path::new(exe), Path::new(data)).unwrap()
}

#[test]
fn the_systemd_unit_runs_as_the_user_and_writes_only_the_data_directory() {
    let unit = serve("/usr/local/bin/davy", "/var/lib/davy")
        .systemd("davy")
        .unwrap();
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
ExecStart=/usr/local/bin/davy serve --data /var/lib/davy
Restart=on-failure
RestartSec=2
LimitNOFILE=65536
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
fn the_systemd_unit_refuses_what_systemd_would_read_as_syntax() {
    let home = serve("/home/ops/bin/davy", "/home/ops/davy")
        .systemd("ops")
        .unwrap();
    assert!(home.contains("\nProtectHome=read-only\n"), "{home}");
    for (exe, data) in [
        ("/usr/bin/davy", "/srv/my data"),
        ("/usr/bin/davy", "/srv/$HOME"),
        ("/usr/bin/davy", "/srv/50%"),
        ("/usr/bin/davy", "relative"),
        ("/opt/\"x\"/davy", "/var/lib/davy"),
    ] {
        let path = if data == "/var/lib/davy" { exe } else { data };
        assert_eq!(
            serve(exe, data).systemd("davy").unwrap_err().to_string(),
            format!(
                "{path:?} has a character systemd reads as syntax, use a path of letters, digits and / . _ + -"
            )
        );
    }
    for user in ["0", "1000", "a b", "-x", "davy\nExecStartPre=/bin/sh", ""] {
        assert_eq!(
            serve("/usr/bin/davy", "/var/lib/davy")
                .systemd(user)
                .unwrap_err()
                .to_string(),
            format!(
                "{user:?} is not a user name, use letters, digits and _ . - starting with a letter"
            )
        );
    }
}

#[test]
fn a_line_break_in_a_path_is_refused() {
    let err = Serve::new(Path::new("/usr/bin/davy"), Path::new("/srv/a\nb"))
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "\"/srv/a\\nb\" has a control character, choose a path without one"
    );
}

#[test]
fn root_and_unknown_users_are_refused() {
    let not_root = "the service must not run as root. Create a user for it (docs/install.md, step 3) and run setup as that user with `runuser -u davy -- davy setup ...`";
    assert_eq!(check_user("root").unwrap_err().to_string(), not_root);
    #[cfg(unix)]
    {
        assert_eq!(
            check_user("no-such-davy-user").unwrap_err().to_string(),
            "there is no user no-such-davy-user, create it (docs/install.md, step 3)"
        );
        let me = nix::unistd::User::from_uid(nix::unistd::geteuid())
            .unwrap()
            .unwrap();
        assert_eq!(check_user(&me.name).is_ok(), !me.uid.is_root());
    }
}

#[test]
fn the_launchd_job_escapes_xml_and_logs_to_the_data_directory() {
    let plist = serve("/opt/d&vy", "/Users/me/davy").launchd();
    assert_eq!(
        plist,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>davy</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/d&amp;vy</string>
    <string>serve</string>
    <string>--data</string>
    <string>/Users/me/davy</string>
  </array>
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
fn the_shell_line_quotes_words() {
    assert_eq!(
        serve("/usr/local/bin/davy", "/srv/my data").shell(),
        "/usr/local/bin/davy serve --data '/srv/my data'"
    );
}
