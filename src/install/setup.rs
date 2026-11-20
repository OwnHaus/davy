//! `davy setup` prepares a data directory and the service that serves it.
//! Running it again with the same flags changes nothing and prints the same
//! JSON, so an agent may retry it after any failure.

use super::{
    SETTINGS_FILE, SMTP_FILE, Settings, TOKEN_FILE, check_public_url, sh,
    unit::{Serve, check_user},
};
use crate::secret::{self, MIN_TOKEN_CHARS};
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
    /// How the server is started: the commands for a systemd unit, a launchd
    /// job written into the data directory, or a shell command.
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
    let exe = super::exe()?;
    let user = std::env::var("USER").ok();
    let report = setup(args, &exe, user.as_deref())?;
    serde_json::to_writer_pretty(&mut *out, &report)?;
    writeln!(out)?;
    Ok(())
}

/// Prepares `args.data` for a server started from `exe` by `user`.
pub fn setup(args: &Args, exe: &Path, user: Option<&str>) -> anyhow::Result<Report> {
    check_public_url(&args.public_url)?;
    args.listen
        .parse::<std::net::SocketAddr>()
        .with_context(|| {
            format!(
                "--listen {:?} is not an address such as 127.0.0.1:8000",
                args.listen
            )
        })?;
    if args.service == Service::Systemd {
        check_user(
            user.context("USER is not set, run setup as the user the service will run as")?,
        )?;
    }
    refuse_foreign_dir(&args.data)?;
    std::fs::create_dir_all(&args.data)
        .with_context(|| format!("cannot create {}", args.data.display()))?;
    let data = args.data.canonicalize()?;
    check_owner(&data)?;
    restrict(&data, 0o700)?;
    let token_file = data.join(TOKEN_FILE);
    ensure_token(&token_file)?;
    let smtp_file = data.join(SMTP_FILE);
    let smtp = regular_file(&smtp_file)?;
    if smtp {
        restrict(&smtp_file, 0o600)?;
    }
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
    let (service_file, next) = service(args.service, exe, &data, user)?;
    Ok(Report {
        data,
        token_file,
        smtp,
        service_file,
        next,
    })
}

/// The service file setup writes, if any, and the commands to run next.
fn service(
    service: Service,
    exe: &Path,
    data: &Path,
    user: Option<&str>,
) -> anyhow::Result<(Option<PathBuf>, Vec<String>)> {
    let serve = Serve::new(exe, data)?;
    let (service_file, mut next) = match service {
        Service::Systemd => {
            let unit = format!(
                "{} systemd-unit --data {} --user {}",
                sh(&exe.to_string_lossy()),
                sh(&data.to_string_lossy()),
                sh(user.unwrap_or_default())
            );
            let next = vec![
                format!(
                    "unit=\"$({unit})\" && printf '%s\\n' \"$unit\" | sudo tee /etc/systemd/system/davy.service > /dev/null"
                ),
                "sudo systemctl daemon-reload".to_owned(),
                "sudo systemctl enable davy".to_owned(),
                "sudo systemctl restart davy".to_owned(),
            ];
            (None, next)
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
    let doctor = format!(
        "{} doctor --data {}",
        sh(&exe.to_string_lossy()),
        sh(&data.to_string_lossy())
    );
    next.push(match (service, user) {
        (Service::Systemd, Some(user)) => format!("sudo runuser -u {} -- {doctor}", sh(user)),
        _ => doctor,
    });
    Ok((service_file, next))
}

/// Setup makes the directory private, so it refuses a directory that holds
/// files and none of Davy's, such as `/var/lib` given by mistake.
fn refuse_foreign_dir(dir: &Path) -> anyhow::Result<()> {
    let Ok(mut entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let empty = entries.next().is_none();
    let ours = [TOKEN_FILE, SETTINGS_FILE, SMTP_FILE, crate::store::DB_FILE]
        .iter()
        .any(|f| dir.join(f).exists());
    ensure!(
        empty || ours,
        "{} holds other files, give setup a new directory such as /var/lib/davy",
        dir.display()
    );
    Ok(())
}

/// Setup makes the directory private to the user who runs it, so it refuses
/// one that belongs to someone else.
#[cfg(unix)]
fn check_owner(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let owner = std::fs::metadata(dir)?.uid();
    ensure!(
        owner == nix::unistd::geteuid().as_raw(),
        "{} belongs to another user, run setup as its owner (docs/install.md, step 4)",
        dir.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn check_owner(_dir: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Whether `path` is a regular file. A symbolic link or a directory is
/// refused, so setup never writes or tightens a file somewhere else.
fn regular_file(path: &Path) -> anyhow::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => Ok(true),
        Ok(_) => bail!(
            "{} is not a regular file, replace it with one",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Keeps a token that is long enough and writes a new one when there is none.
fn ensure_token(path: &Path) -> anyhow::Result<()> {
    if regular_file(path)? {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if let Some(chars) = secret::too_short(secret::line(&text)) {
            bail!(
                "{} holds {chars} characters and needs {MIN_TOKEN_CHARS}. Delete it and run setup again for a new token, then update every agent and sentry-cli that used the old one",
                path.display()
            );
        }
        return restrict(path, 0o600);
    }
    let token =
        secret::random_hex().map_err(|e| anyhow::anyhow!("no randomness for the token: {e}"))?;
    let mut file = new_private_file(path)?;
    writeln!(file, "{token}")?;
    Ok(())
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

/// Replaces `path` with a private file holding `contents`. A file of that
/// name that is a link is replaced, not followed.
fn write(path: &Path, contents: &str) -> anyhow::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.{}", uuid::Uuid::new_v4()));
    let mut file = new_private_file(&temp)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temp, path).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
