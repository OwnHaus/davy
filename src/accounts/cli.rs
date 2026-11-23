//! `davy user`: the people who may use the web UI. Writes from its own
//! process, like the other commands that change the database.

use super::{Email, INVITE_MS, Secret, store};
use crate::{install, time};
use clap::{Args, Subcommand};
use std::{io::Write, path::PathBuf};

#[derive(Subcommand)]
pub enum User {
    /// Adds a person and prints a link, valid for 24 hours and once, where
    /// they set a password.
    Add(Invite),
    /// Takes away a person's password and sessions and prints a new link.
    Reset(Invite),
    /// Removes a person and ends their sessions.
    Remove {
        email: String,
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Lists the people, and whether each has set a password.
    List {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
pub struct Invite {
    email: String,
    #[arg(long)]
    data: PathBuf,
    /// Defaults to the public URL given to `davy setup` for this directory.
    #[arg(long)]
    public_url: Option<url::Url>,
    #[arg(long)]
    json: bool,
}

pub fn run(out: &mut impl Write, command: User) -> anyhow::Result<()> {
    match command {
        User::Add(args) => invite(out, &args, false),
        User::Reset(args) => invite(out, &args, true),
        User::Remove { email, data, json } => {
            let email = Email::parse(&email).map_err(anyhow::Error::msg)?;
            anyhow::ensure!(
                store::remove(&crate::store::open(&data)?, &email)?,
                "no user {email}, see davy user list --data {}",
                install::sh(&data.to_string_lossy())
            );
            if json {
                writeln!(out, "{}", serde_json::json!({ "removed": email.as_str() }))?;
            }
            Ok(())
        }
        User::List { data, json } => {
            let users = store::list(&crate::store::open(&data)?)?;
            if json {
                writeln!(out, "{}", serde_json::to_string(&users)?)?;
                return Ok(());
            }
            for u in users {
                let state = if u.active { "active" } else { "invited" };
                writeln!(out, "{}\t{}\t{state}", u.id, u.email)?;
            }
            Ok(())
        }
    }
}

fn invite(out: &mut impl Write, args: &Invite, reset: bool) -> anyhow::Result<()> {
    let email = Email::parse(&args.email).map_err(anyhow::Error::msg)?;
    let public_url = match &args.public_url {
        Some(url) => url.clone(),
        None => install::Settings::load(&args.data)?.public_url()?,
    };
    install::check_public_url(&public_url)?;
    // The session cookie is Secure, so a browser keeps it only from https
    // or this machine. A link to plain http elsewhere would also send the
    // new password in the clear.
    anyhow::ensure!(
        install::encrypted_or_local(&public_url) || public_url.host_str() == Some("localhost"),
        "the web UI needs https at {public_url}, set an https public URL or put Davy behind a TLS proxy"
    );
    let c = crate::store::open(&args.data)?;
    let secret = Secret::random()?;
    let now = time::now_ms();
    if reset {
        anyhow::ensure!(
            store::reset(&c, &email, &secret.hash(), now)?,
            "no user {email}, add one with davy user add {} --data {}",
            install::sh(email.as_str()),
            install::sh(&args.data.to_string_lossy())
        );
    } else {
        anyhow::ensure!(
            store::add(&c, &email, &secret.hash(), now)?.is_some(),
            "{email} already has a user, send a new link with davy user reset {} --data {}",
            install::sh(email.as_str()),
            install::sh(&args.data.to_string_lossy())
        );
    }
    let link = welcome_link(&public_url, &secret);
    if args.json {
        let expires_at = time::rfc3339_ms(now + INVITE_MS);
        let v =
            serde_json::json!({ "email": email.as_str(), "link": link, "expires_at": expires_at });
        writeln!(out, "{v}")?;
    } else {
        writeln!(out, "{link}")?;
    }
    Ok(())
}

/// `<public URL>/ui/welcome#<secret>`. The secret is in the fragment, which
/// a browser sends to no server and no log.
pub fn welcome_link(public_url: &url::Url, secret: &Secret) -> String {
    let base = public_url.as_str().trim_end_matches('/');
    format!("{base}/ui/welcome#{}", secret.encode())
}
