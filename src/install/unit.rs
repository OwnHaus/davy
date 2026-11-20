//! The service files that start `davy serve --data DIR`. They name the
//! binary, the data directory and, for systemd, the user, and nothing read
//! from the data directory, so the service user cannot change what root
//! installs.

use super::sh;
use anyhow::{Context as _, ensure};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// The data directory that `davy setup` prepared.
    #[arg(long)]
    pub data: PathBuf,
    /// The user the service runs as, who ran `davy setup`.
    #[arg(long)]
    pub user: String,
}

pub fn run(out: &mut impl Write, args: &Args) -> anyhow::Result<()> {
    let exe = super::exe()?;
    let data = args
        .data
        .canonicalize()
        .with_context(|| format!("cannot find {}, run davy setup first", args.data.display()))?;
    check_user(&args.user)?;
    write!(out, "{}", Serve::new(&exe, &data)?.systemd(&args.user)?)?;
    Ok(())
}

const NOT_ROOT: &str = "the service must not run as root. Create a user for it (docs/install.md, step 3) and run setup as that user with `runuser -u davy -- davy setup ...`";

/// Refuses a user that does not exist or is root, since the server parses
/// untrusted input.
pub fn check_user(user: &str) -> anyhow::Result<()> {
    user_name(user)?;
    ensure!(user != "root", "{NOT_ROOT}");
    #[cfg(unix)]
    {
        let found = nix::unistd::User::from_name(user)
            .with_context(|| format!("cannot look up the user {user}"))?
            .with_context(|| {
                format!("there is no user {user}, create it (docs/install.md, step 3)")
            })?;
        ensure!(!found.uid.is_root(), "{NOT_ROOT}");
    }
    Ok(())
}

/// Refuses a name systemd would read as a number or as syntax.
fn user_name(user: &str) -> anyhow::Result<()> {
    let mut chars = user.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        && user.len() <= 32;
    ensure!(
        plain,
        "{user:?} is not a user name, use letters, digits and _ . - starting with a letter"
    );
    Ok(())
}

/// How the service starts: `EXE serve --data DATA`.
pub struct Serve {
    exe: String,
    data: String,
}

impl Serve {
    /// Refuses a control character in either path, because a line break would
    /// start a new directive in a unit file.
    pub fn new(exe: &Path, data: &Path) -> anyhow::Result<Self> {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let serve = Self {
            exe: text(exe),
            data: text(data),
        };
        for word in [&serve.exe, &serve.data] {
            ensure!(
                !word.chars().any(char::is_control),
                "{word:?} has a control character, choose a path without one"
            );
        }
        Ok(serve)
    }

    fn words(&self) -> [&str; 4] {
        [&self.exe, "serve", "--data", &self.data]
    }

    /// One line to paste into a shell.
    pub fn shell(&self) -> String {
        self.words().map(sh).join(" ")
    }

    /// A unit for `/etc/systemd/system/davy.service` that runs as `user` with
    /// write access to the data directory only.
    /// Refuses a path with a character systemd would read as syntax, so no
    /// word needs quoting or escaping.
    pub fn systemd(&self, user: &str) -> anyhow::Result<String> {
        user_name(user)?;
        for path in [&self.exe, &self.data] {
            ensure!(
                path.starts_with('/')
                    && path
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c)),
                "{path:?} has a character systemd reads as syntax, use a path of letters, digits and / . _ + -"
            );
        }
        let exec = self.words().join(" ");
        let home = if [&self.exe, &self.data].iter().any(|p| under_home(p)) {
            "read-only"
        } else {
            "yes"
        };
        Ok(format!(
            "[Unit]\n\
             Description=Davy error tracking, logs and uptime\n\
             Documentation=https://github.com/dominikzabcik/davy\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             Type=notify\n\
             User={user}\n\
             ExecStart={exec}\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             LimitNOFILE=65536\n\
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
            data = self.data,
        ))
    }

    /// A launchd job for `~/Library/LaunchAgents/davy.plist` that logs to
    /// `DATA/davy.log`.
    pub fn launchd(&self) -> String {
        let string = |s: &str| format!("<string>{}</string>", xml(s));
        let args = self
            .words()
            .map(|w| format!("\n    {}", string(w)))
            .concat();
        let log = string(&format!("{}/davy.log", self.data));
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n  \
             <key>Label</key><string>davy</string>\n  \
             <key>ProgramArguments</key>\n  <array>{args}\n  </array>\n  \
             <key>RunAtLoad</key><true/>\n  \
             <key>KeepAlive</key><true/>\n  \
             <key>StandardOutPath</key>{log}\n  \
             <key>StandardErrorPath</key>{log}\n\
             </dict>\n\
             </plist>\n"
        )
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
