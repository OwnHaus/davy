//! `davy serve`. A flag that is not given comes from what `davy setup` wrote
//! into the data directory, so a service file needs no more than `--data`
//! and holds nothing a less trusted user could have chosen.

use crate::{
    alert, http,
    install::{self, SMTP_FILE, Settings, TOKEN_FILE},
    retention, secret,
    token::Token,
};
use std::path::{Path, PathBuf};
use url::Url;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    pub data: PathBuf,
    /// Defaults to the address in DATA/setup.json.
    #[arg(long)]
    pub listen: Option<String>,
    /// Defaults to the public URL in DATA/setup.json.
    #[arg(long)]
    pub public_url: Option<Url>,
    /// Days of logs a project keeps unless it has a setting of its own, 1 to
    /// 36,500. Change one project with `davy project retention`. Lowering
    /// it deletes the older logs of every project without a setting of its
    /// own, within a minute and for good.
    #[arg(long, default_value_t = retention::DEFAULT_LOG_DAYS)]
    pub log_retention_days: u32,
    /// Days of errors a project keeps unless it has a setting of its own, 1
    /// to 36,500, by when Davy received them. The default keeps them all.
    /// Lowering it deletes the older errors of every project without a
    /// setting of its own, within 10 s and for good.
    #[arg(long, default_value_t = retention::DEFAULT_ERROR_DAYS)]
    pub error_retention_days: u32,
    /// The sender of alert e-mail, required when there is an SMTP URL.
    /// Defaults to the sender in DATA/setup.json.
    #[arg(long)]
    pub mail_from: Option<String>,
    /// Do not record Davy's own warnings, errors and crashes in Davy's own
    /// project. They still go to stderr.
    #[arg(long)]
    pub no_self_report: bool,
}

/// What the server listens on and where it is reached.
#[derive(Debug, PartialEq)]
pub struct Resolved {
    pub listen: String,
    pub public_url: Url,
    pub mail_from: Option<String>,
}

/// Fills the flags that are not given from DATA/setup.json. The file is read
/// only when a flag is missing.
pub fn resolve(args: &Args) -> anyhow::Result<Resolved> {
    let saved = match (&args.listen, &args.public_url) {
        (Some(_), Some(_)) => None,
        _ => Some(Settings::load(&args.data)?),
    };
    let (listen, public_url, mail_from) = match saved {
        None => (args.listen.clone(), args.public_url.clone(), None),
        Some(s) => (
            Some(args.listen.clone().unwrap_or(s.listen.clone())),
            Some(match &args.public_url {
                Some(url) => url.clone(),
                None => s.public_url()?,
            }),
            s.mail_from,
        ),
    };
    let (Some(listen), Some(public_url)) = (listen, public_url) else {
        anyhow::bail!("pass --listen and --public-url, or run davy setup first");
    };
    install::check_public_url(&public_url)?;
    Ok(Resolved {
        listen,
        public_url,
        mail_from: args.mail_from.clone().or(mail_from),
    })
}

/// The environment, where `DAVY_TOKEN_FILE` and `DAVY_SMTP_URL_FILE` default
/// to the files `davy setup` keeps in `data` when they exist and neither the
/// variable nor its `_FILE` form is set.
pub fn env(data: &Path, var: impl Fn(&str) -> Option<String>) -> impl Fn(&str) -> Option<String> {
    let defaults = [
        ("DAVY_TOKEN", data.join(TOKEN_FILE)),
        ("DAVY_SMTP_URL", data.join(SMTP_FILE)),
    ];
    move |key: &str| {
        let set = |k: &str| var(k).filter(|v| !v.is_empty());
        set(key).or_else(|| {
            let (name, path) = defaults
                .iter()
                .find(|(name, _)| key.strip_suffix("_FILE") == Some(*name))?;
            (set(name).is_none() && path.exists()).then(|| path.to_string_lossy().into_owned())
        })
    }
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    if !args.no_self_report {
        crate::self_report::crash::install_hook(&args.data);
    }
    let settings = resolve(&args)?;
    let days =
        |flag: &str, n| retention::Days::new(n).map_err(|e| anyhow::anyhow!("--{flag}: {e}"));
    let retention = retention::Defaults {
        logs: days("log-retention-days", args.log_retention_days)?,
        errors: days("error-retention-days", args.error_retention_days)?,
        hot_days: retention::HotDays::OFF,
    };
    let env = env(&args.data, |key| std::env::var(key).ok());
    let token = secret::admin_token(&env)?;
    let smtp = match (secret::read("DAVY_SMTP_URL", &env)?, &settings.mail_from) {
        (Some(url), Some(from)) => Some(alert::mail::Smtp::new(&url, from)?),
        (Some(_), None) => anyhow::bail!("the SMTP URL needs --mail-from"),
        (None, Some(_)) => anyhow::bail!("--mail-from needs DAVY_SMTP_URL or DATA/smtp-url"),
        (None, None) => None,
    };
    http::serve(
        &args.data,
        &settings.listen,
        &settings.public_url,
        retention,
        token.as_deref().map(Token::new),
        smtp,
        !args.no_self_report,
    )
    .await
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
