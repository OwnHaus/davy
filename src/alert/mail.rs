//! Sends e-mail rows through the operator's SMTP server.

use super::Mail;
use anyhow::{Context as _, bail};
use lettre::{
    Address, AsyncSmtpTransport, AsyncTransport as _, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
};
use std::time::Duration;
use url::Url;

const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ERROR_CHARS: usize = 200;

/// The SMTP server and sender of every e-mail channel.
#[derive(Clone)]
pub struct Smtp {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl std::fmt::Debug for Smtp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Smtp")
            .field("from", &self.from)
            .finish_non_exhaustive()
    }
}

impl Smtp {
    /// Reads `DAVY_SMTP_URL` and the `--mail-from` address. A password is
    /// only accepted over TLS, from the first byte or by required STARTTLS.
    /// An error never holds the URL, because the URL holds the password.
    pub fn new(url: &str, from: &str) -> anyhow::Result<Self> {
        let parsed =
            Url::parse(url).map_err(|e| anyhow::anyhow!("DAVY_SMTP_URL is not a URL: {e}"))?;
        let tls = parsed
            .query_pairs()
            .find(|(k, _)| k == "tls")
            .map(|(_, v)| v.into_owned());
        let secure = match (parsed.scheme(), tls.as_deref()) {
            ("smtps", None) | ("smtp", Some("required")) => true,
            ("smtp", None) => false,
            _ => bail!(
                "DAVY_SMTP_URL must be smtps://, smtp://...?tls=required or smtp:// without a password"
            ),
        };
        if !secure && (!parsed.username().is_empty() || parsed.password().is_some()) {
            bail!("DAVY_SMTP_URL sends a password only over TLS, use smtps:// or ?tls=required");
        }
        let from: Mailbox = from
            .parse()
            .map_err(|e| anyhow::anyhow!("--mail-from is not an address: {e}"))?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|e| anyhow::anyhow!("DAVY_SMTP_URL was refused: {}", text(&e)))?
            .build();
        Ok(Self { transport, from })
    }

    /// Sends the `Mail` in `payload` to `to`. The error is the server's
    /// answer or the connection error, cut to 200 characters.
    pub async fn send(&self, to: &str, payload: &str) -> Result<(), String> {
        let message = self.message(to, payload).map_err(|e| format!("{e:#}"))?;
        match tokio::time::timeout(SEND_TIMEOUT, self.transport.send(message)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(text(&e)),
            Err(_) => Err("timeout".into()),
        }
    }

    fn message(&self, to: &str, payload: &str) -> anyhow::Result<Message> {
        let mail: Mail =
            serde_json::from_str(payload).context("the e-mail payload is not valid")?;
        let to: Address = to.parse().context("the e-mail address is not valid")?;
        Ok(Message::builder()
            .from(self.from.clone())
            .to(Mailbox::new(None, to))
            .subject(mail.subject)
            .message_id(Some(format!(
                "<davy.{}@{}>",
                mail.message_id,
                self.from.email.domain()
            )))
            .header(ContentType::TEXT_PLAIN)
            .body(mail.text)?)
    }
}

/// lettre's error text already ends with its cause.
fn text(e: &lettre::transport::smtp::Error) -> String {
    e.to_string().chars().take(MAX_ERROR_CHARS).collect()
}
