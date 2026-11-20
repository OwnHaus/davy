//! `davy setup` prepares a data directory and the service that serves it.
//! Running it again with the same flags changes nothing and prints the same
//! JSON, so an agent may retry it after any failure.

use super::{SETTINGS_FILE, SMTP_FILE, Settings, TOKEN_FILE, check_public_url, sh, unit::Serve};
use crate::secret::MIN_TOKEN_CHARS;
use anyhow::{Context as _, bail, ensure};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use url::Url;

#[derive(clap::Args)]
pub struct Args {
    /// The data directory. It is created readable by its owner only.
    #[arg(long)]
    pub data: PathBuf,
    /// Where SDKs and agents reach the server, e.g. https://davy.example.com.
    #[arg(long)]
    pub public_url: Url,
    /// The address the server listens on. Keep it on loopback behind a TLS proxy.
    #[arg(long, default_value = "127.0.0.1:8000")]
    pub listen: String,
    /// The sender of alert e-mail. Write the SMTP URL to DATA/smtp-url first.
    #[arg(long)]
    pub mail_from: Option<String>,
    /// The service file to write into the data directory.
    #[arg(long, value_enum, default_value_t = Service::None)]
    pub service: Service,
}

#[derive(Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
pub enum Service {
    Systemd,
    Launchd,
    None,
}

/// What setup prints. Every path is absolute.
#[derive(Debug, PartialEq, Serialize)]
pub struct Report {
    pub data: PathBuf,
    pub token_file: PathBuf,
    pub smtp: bool,
    pub service_file: Option<PathBuf>,
    /// The commands to run next, in order.
    pub next: Vec<String>,
}

pub fn run(out: &mut impl Write, args: &Args) -> anyhow::Result<()> {
    let exe = std::env::current_exe()
        .and_then(|e| e.canonicalize())
        .context("cannot find the path of this binary")?;
    let user = std::env::var("USER").ok();
    let report = setup(args, &exe, user.as_deref())?;
    serde_json::to_writer_pretty(&mut *out, &report)?;
    writeln!(out)?;
    Ok(())
}

/// Prepares `args.data` for a server started from `exe` by `user`.
pub fn setup(args: &Args, exe: &Path, user: Option<&str>) -> anyhow::Result<Report> {
    check_public_url(&args.public_url)?;
    refuse_foreign_dir(&args.data)?;
    std::fs::create_dir_all(&args.data)
        .with_context(|| format!("cannot create {}", args.data.display()))?;
    let data = args.data.canonicalize()?;
    restrict(&data, 0o700)?;
    let token_file = data.join(TOKEN_FILE);
    ensure_token(&token_file)?;
    let smtp_file = data.join(SMTP_FILE);
    let smtp = smtp_file.exists();
    match (smtp, &args.mail_from) {
        (true, None) => bail!(
            "{} exists, pass --mail-from with the sender of alert e-mail",
            smtp_file.display()
        ),
        (false, Some(_)) => bail!(
            "--mail-from needs the SMTP URL in {}, write it there first (docs/install.md, step 5)",
            smtp_file.display()
        ),
        _ => {}
    }
    let settings = Settings {
        public_url: args.public_url.to_string(),
        listen: args.listen.clone(),
        mail_from: args.mail_from.clone(),
    };
    let mut json = serde_json::to_string_pretty(&settings)?;
    json.push('\n');
    write(&data.join(SETTINGS_FILE), &json)?;
    let serve = Serve::new(exe, &data, &settings, smtp)?;
    let (service_file, mut next) = match args.service {
        Service::Systemd => {
            let file = data.join("davy.service");
            write(&file, &serve.systemd(service_user(user)?))?;
            let next = vec![
                format!(
                    "sudo install -m 644 {} /etc/systemd/system/davy.service",
                    sh(&file.to_string_lossy())
                ),
                "sudo systemctl daemon-reload".to_owned(),
                "sudo systemctl enable davy".to_owned(),
                "sudo systemctl restart davy".to_owned(),
            ];
            (Some(file), next)
        }
        Service::Launchd => {
            let file = data.join("davy.plist");
            write(&file, &serve.launchd())?;
            let next = vec![
                "mkdir -p ~/Library/LaunchAgents".to_owned(),
                format!(
                    "cp {} ~/Library/LaunchAgents/davy.plist",
                    sh(&file.to_string_lossy())
                ),
                "launchctl bootout gui/$(id -u)/davy 2>/dev/null || true".to_owned(),
                "launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/davy.plist".to_owned(),
            ];
            (Some(file), next)
        }
        Service::None => (None, vec![serve.shell()]),
    };
    next.push(format!(
        "{} doctor --data {}",
        sh(&exe.to_string_lossy()),
        sh(&data.to_string_lossy())
    ));
    Ok(Report {
        data,
        token_file,
        smtp,
        service_file,
        next,
    })
}

/// Setup makes the directory private, so it refuses a directory that holds
/// files and none of Davy's, such as `/var/lib` given by mistake.
fn refuse_foreign_dir(dir: &Path) -> anyhow::Result<()> {
    let Ok(mut entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let empty = entries.next().is_none();
    let ours = [TOKEN_FILE, SETTINGS_FILE, SMTP_FILE, "davy.sqlite"]
        .iter()
        .any(|f| dir.join(f).exists());
    ensure!(
        empty || ours,
        "{} holds other files, give setup a new directory such as /var/lib/davy",
        dir.display()
    );
    Ok(())
}

/// systemd runs the server as the user who ran setup, so that user owns the
/// data directory. Root is refused: the server parses untrusted input.
fn service_user(user: Option<&str>) -> anyhow::Result<&str> {
    let user = user
        .filter(|u| !u.is_empty())
        .context("USER is not set, run setup as the user the service will run as")?;
    ensure!(
        user != "root",
        "the service must not run as root. Create a user for it (docs/install.md, step 3) and run setup as that user with `runuser -u davy -- davy setup ...`"
    );
    Ok(user)
}

/// Keeps a token that is long enough and writes a new one when there is none.
fn ensure_token(path: &Path) -> anyhow::Result<()> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let chars = text.trim_end_matches(['\n', '\r']).chars().count();
            ensure!(
                chars >= MIN_TOKEN_CHARS,
                "{} holds {chars} characters and needs {MIN_TOKEN_CHARS}. Delete it and run setup again for a new token, then update every agent and sentry-cli that used the old one",
                path.display()
            );
            restrict(path, 0o600)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut bytes = [0_u8; 32];
            getrandom::fill(&mut bytes)
                .map_err(|e| anyhow::anyhow!("no randomness for the token: {e}"))?;
            let mut file = new_private_file(path)?;
            writeln!(file, "{}", crate::alert::hex(&bytes))?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn new_private_file(path: &Path) -> anyhow::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))
}

/// Sets the permission bits of `path`, so a second run tightens what was
/// opened by hand or created under a loose umask.
#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).with_context(|| {
        format!(
            "cannot make {} private, run setup as the user who owns it",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> anyhow::Result<()> {
    Ok(())
}

fn write(path: &Path, contents: &str) -> anyhow::Result<()> {
    std::fs::write(path, contents).with_context(|| format!("cannot write {}", path.display()))?;
    restrict(path, 0o600)
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
