//! The `davy serve` command line that `davy setup` writes into a systemd unit,
//! a launchd job or a shell command.

use super::{SMTP_FILE, Settings, TOKEN_FILE, sh};
use anyhow::ensure;
use std::path::Path;

/// How the service starts: the binary, its arguments and the variables that
/// name the secret files. No word holds a secret.
pub struct Serve {
    exe: String,
    data: String,
    args: Vec<String>,
    env: Vec<(&'static str, String)>,
}

impl Serve {
    /// Refuses a control character in any word, because a line break would
    /// start a new directive in the unit file.
    pub fn new(exe: &Path, data: &Path, settings: &Settings, smtp: bool) -> anyhow::Result<Self> {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let mut args = vec![
            "serve".to_owned(),
            "--data".to_owned(),
            text(data),
            "--listen".to_owned(),
            settings.listen.clone(),
            "--public-url".to_owned(),
            settings.public_url.clone(),
        ];
        if let Some(from) = &settings.mail_from {
            args.extend(["--mail-from".to_owned(), from.clone()]);
        }
        let mut env = vec![("DAVY_TOKEN_FILE", text(&data.join(TOKEN_FILE)))];
        if smtp {
            env.push(("DAVY_SMTP_URL_FILE", text(&data.join(SMTP_FILE))));
        }
        let serve = Self {
            exe: text(exe),
            data: text(data),
            args,
            env,
        };
        let words = std::iter::once(&serve.exe)
            .chain(&serve.args)
            .chain(serve.env.iter().map(|(_, v)| v));
        for word in words {
            ensure!(
                !word.chars().any(char::is_control),
                "{word:?} has a control character, choose a path or value without one"
            );
        }
        Ok(serve)
    }

    /// One line to paste into a shell.
    pub fn shell(&self) -> String {
        let env = self.env.iter().map(|(k, v)| format!("{k}={}", sh(v)));
        let cmd = std::iter::once(&self.exe).chain(&self.args).map(|w| sh(w));
        env.chain(cmd).collect::<Vec<_>>().join(" ")
    }

    /// A unit for `/etc/systemd/system/davy.service` that runs as `user` with
    /// write access to the data directory only.
    pub fn systemd(&self, user: &str) -> String {
        let exec = std::iter::once(&self.exe)
            .chain(&self.args)
            .map(|w| unit_word(w))
            .collect::<Vec<_>>()
            .join(" ");
        let env = self
            .env
            .iter()
            .map(|(k, v)| format!("Environment={}\n", unit_word(&format!("{k}={v}"))))
            .collect::<Vec<_>>()
            .concat();
        let home = if [&self.exe, &self.data].iter().any(|p| under_home(p)) {
            "read-only"
        } else {
            "yes"
        };
        format!(
            "[Unit]\n\
             Description=Davy error tracking, logs and uptime\n\
             Documentation=https://github.com/dominikzabcik/davy\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             User={user}\n\
             ExecStart={exec}\n\
             {env}\
             Restart=on-failure\n\
             RestartSec=2\n\
             UMask=0077\n\
             NoNewPrivileges=yes\n\
             ProtectSystem=strict\n\
             ProtectHome={home}\n\
             ReadWritePaths={data}\n\
             PrivateTmp=yes\n\
             PrivateDevices=yes\n\
             ProtectKernelTunables=yes\n\
             ProtectKernelModules=yes\n\
             ProtectKernelLogs=yes\n\
             ProtectControlGroups=yes\n\
             RestrictNamespaces=yes\n\
             RestrictRealtime=yes\n\
             RestrictSUIDSGID=yes\n\
             LockPersonality=yes\n\
             MemoryDenyWriteExecute=yes\n\
             SystemCallArchitectures=native\n\
             RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\n\
             CapabilityBoundingSet=\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n",
            user = unit_word(user),
            data = unit_word(&self.data),
        )
    }

    /// A launchd job for `~/Library/LaunchAgents/davy.plist` that logs to
    /// `DATA/davy.log`.
    pub fn launchd(&self) -> String {
        let string = |s: &str| format!("<string>{}</string>", xml(s));
        let args = std::iter::once(&self.exe)
            .chain(&self.args)
            .map(|w| format!("\n    {}", string(w)))
            .collect::<Vec<_>>()
            .concat();
        let env = self
            .env
            .iter()
            .map(|(k, v)| format!("\n    <key>{k}</key>{}", string(v)))
            .collect::<Vec<_>>()
            .concat();
        let log = string(&format!("{}/davy.log", self.data));
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n  \
             <key>Label</key><string>davy</string>\n  \
             <key>ProgramArguments</key>\n  <array>{args}\n  </array>\n  \
             <key>EnvironmentVariables</key>\n  <dict>{env}\n  </dict>\n  \
             <key>RunAtLoad</key><true/>\n  \
             <key>KeepAlive</key><true/>\n  \
             <key>StandardOutPath</key>{log}\n  \
             <key>StandardErrorPath</key>{log}\n\
             </dict>\n\
             </plist>\n"
        )
    }
}

/// systemd expands `%` specifiers and `$` variables in a command line, and
/// splits words at spaces unless they are quoted.
fn unit_word(s: &str) -> String {
    let escaped = s.replace('%', "%%").replace('$', "$$");
    if escaped
        .chars()
        .any(|c| c.is_whitespace() || "\"'\\;".contains(c))
    {
        format!("\"{}\"", escaped.replace('\\', r"\\").replace('"', "\\\""))
    } else {
        escaped
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `ProtectHome=yes` hides these, so a binary or data directory below them
/// needs `read-only` instead.
fn under_home(path: &str) -> bool {
    ["/home", "/root", "/run/user"]
        .iter()
        .any(|home| Path::new(path).starts_with(home))
}

#[cfg(test)]
#[path = "unit_tests.rs"]
mod tests;
