//! Outgoing email: confirming addresses and password-reset links, sent over
//! SMTP (lettre, with rustls and the system's certificates).
//!
//! Sending happens in a background task, so a response never waits on the
//! mail server and its timing does not reveal whether an account exists.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::{EmailConfig, SmtpSecurity};

/// How long to wait for the mail server.
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

/// A message as the in-memory mailer records it (tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentEmail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[derive(Clone)]
pub enum Mailer {
    /// No email configured: addresses are still kept, but nothing is sent
    /// and password reset is unavailable.
    Disabled,
    Smtp {
        transport: AsyncSmtpTransport<Tokio1Executor>,
        from: Mailbox,
    },
    /// Records messages instead of sending them (tests).
    Memory(Arc<Mutex<Vec<SentEmail>>>),
}

impl std::fmt::Debug for Mailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disabled => "Mailer::Disabled",
            Self::Smtp { .. } => "Mailer::Smtp",
            Self::Memory(_) => "Mailer::Memory",
        })
    }
}

impl Mailer {
    /// Checks the `[email]` settings without connecting; returns the sender.
    pub fn check_config(c: &EmailConfig) -> Result<Option<Mailbox>, String> {
        let (Some(_), Some(from)) = (c.smtp_host.as_deref(), c.from.as_deref()) else {
            return Ok(None);
        };
        from.parse()
            .map(Some)
            .map_err(|e| format!("email.from is not an email address: {e}"))
    }

    /// The mailer the configuration describes (`Disabled` without an
    /// `[email]` section). Must be called, and the mailer dropped, inside
    /// the Tokio runtime (its connection pool runs there).
    pub fn from_config(c: &EmailConfig) -> Result<Self, String> {
        let (Some(host), Some(from)) = (c.smtp_host.as_deref(), Self::check_config(c)?) else {
            return Ok(Self::Disabled);
        };
        let builder = match c.smtp_security {
            SmtpSecurity::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host),
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
            SmtpSecurity::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                host,
            )),
        }
        .map_err(|e| format!("email: could not set up the connection to {host}: {e}"))?;
        let mut builder = builder.port(c.port()).timeout(Some(SMTP_TIMEOUT));
        if let Some(user) = &c.smtp_username {
            builder = builder.credentials(Credentials::new(
                user.clone(),
                c.smtp_password.clone().unwrap_or_default(),
            ));
        }
        Ok(Self::Smtp {
            transport: builder.build(),
            from,
        })
    }

    pub fn memory() -> (Self, Arc<Mutex<Vec<SentEmail>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (Self::Memory(Arc::clone(&sent)), sent)
    }

    /// Whether this server can send email (and so offers password reset).
    pub fn enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Sends a plain-text message.
    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), String> {
        match self {
            Self::Disabled => Err("this server has no email configured".into()),
            Self::Memory(sent) => {
                sent.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(SentEmail {
                        to: to.to_owned(),
                        subject: subject.to_owned(),
                        body,
                    });
                Ok(())
            }
            Self::Smtp { transport, from } => {
                let to: Mailbox = to
                    .parse()
                    .map_err(|e| format!("not an email address: {e}"))?;
                let message = Message::builder()
                    .from(from.clone())
                    .to(to)
                    .subject(subject)
                    .header(ContentType::TEXT_PLAIN)
                    .body(body)
                    .map_err(|e| format!("could not build the message: {e}"))?;
                transport
                    .send(message)
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("the mail server did not accept the message: {e}"))
            }
        }
    }

    /// Sends in the background; failures go to the server log.
    pub fn send_later(&self, to: String, subject: &'static str, body: String) {
        let mailer = self.clone();
        tokio::spawn(async move {
            match mailer.send(&to, subject, body).await {
                Ok(()) => tracing::info!("sent email: {subject}"),
                Err(e) => tracing::warn!("could not send email ({subject}): {e}"),
            }
        });
    }
}

/// Checks an address someone typed: one `@`, something before it, a dotted
/// domain after it, no spaces, and a length mail systems accept. Returns
/// it trimmed.
pub fn validate_email(email: &str) -> Result<String, &'static str> {
    let email = email.trim();
    const BAD: &str = "Enter an email address like name@example.com.";
    if email.is_empty() {
        return Err("Enter an email address. It is used only to reset a forgotten password.");
    }
    if email.len() > 254 {
        return Err("An email address must be at most 254 characters long.");
    }
    let Some((local, domain)) = email.split_once('@') else {
        return Err(BAD);
    };
    if local.is_empty()
        || domain.contains('@')
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || email.chars().any(|c| c.is_whitespace() || c.is_control())
        || email.parse::<Mailbox>().is_err()
    {
        return Err(BAD);
    }
    Ok(email.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_addresses() {
        assert_eq!(
            validate_email(" Name@Example.com ").as_deref(),
            Ok("Name@Example.com")
        );
        assert!(validate_email("a.b+c@mail.example.org").is_ok());
        for bad in [
            "",
            "name",
            "@example.com",
            "name@",
            "name@localhost",
            "name@@example.com",
            "na me@example.com",
            "name@example.com.",
            "Name <name@example.com>",
        ] {
            assert!(validate_email(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn configured_or_not() {
        assert!(
            !Mailer::from_config(&EmailConfig::default())
                .unwrap()
                .enabled()
        );
        let c = EmailConfig {
            from: Some("AudioNet <no-reply@example.com>".into()),
            smtp_host: Some("smtp.example.com".into()),
            ..EmailConfig::default()
        };
        assert!(Mailer::from_config(&c).unwrap().enabled());
        let bad = EmailConfig {
            from: Some("not an address".into()),
            ..c
        };
        assert!(Mailer::from_config(&bad).is_err());
    }
}
