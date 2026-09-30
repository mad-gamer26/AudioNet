//! Server configuration: a TOML file, with environment-variable overrides
//! for secrets and deployment-specific values.
//!
//! Nothing here defaults to a particular host. Every deployment sets its own
//! `public_url`; see `deploy/config.example.toml`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The URL users and devices use to reach this server, e.g.
    /// `https://audionet.example.com`. Determines cookie security and the
    /// default allowed origin.
    pub public_url: String,
    /// Address the HTTP server listens on (normally behind a reverse proxy).
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    /// SQLite database file.
    #[serde(default = "default_database")]
    pub database: PathBuf,
    /// Directory holding the web client. Omit to serve only the API.
    #[serde(default)]
    pub web_root: Option<PathBuf>,
    /// Directory of downloadable AudioNet builds, served at `/downloads/`
    /// and linked from the web client. Omit to offer no downloads.
    #[serde(default)]
    pub downloads_dir: Option<PathBuf>,
    /// Origins allowed to use cookie-authenticated requests and the
    /// WebSocket. Defaults to the origin of `public_url`.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Whether anyone can create an account (web client, apps, CLI).
    #[serde(default)]
    pub allow_registration: bool,
    /// New accounts one client address may create per hour.
    #[serde(default = "default_sign_ups_per_address")]
    pub sign_ups_per_address_per_hour: u32,
    /// New accounts the whole server accepts per hour.
    #[serde(default = "default_sign_ups_total")]
    pub sign_ups_per_hour: u32,
    /// Request header carrying the client's address, set by the reverse
    /// proxy in front of this server (nginx: `X-Real-IP`, set from
    /// `$remote_addr`). Only name a header the proxy always overwrites.
    /// Unset: the connection's own address (behind a proxy, that is the
    /// proxy, so the per-address sign-up limit acts server-wide).
    #[serde(default)]
    pub client_address_header: Option<String>,
    /// Web sign-in lifetime.
    #[serde(default = "default_session_days")]
    pub session_days: u32,
    #[serde(default)]
    pub ice: IceConfig,
    /// Outgoing email for confirming addresses and resetting passwords.
    #[serde(default)]
    pub email: EmailConfig,
    /// The push gateway this server sends notifications through (and that
    /// it tells apps to register with), e.g. `https://audionet.example.com`.
    /// Unset: no push notifications. The only gateway it ever sends to.
    #[serde(default)]
    pub push_gateway_url: Option<String>,
    /// Running a push gateway here: only on the server whose operator holds
    /// the app's APNs key.
    #[serde(default)]
    pub push_gateway: PushGatewayConfig,
}

/// A push gateway: relays notifications from AudioNet servers to Apple's
/// push service with the app's APNs key.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushGatewayConfig {
    /// The APNs authentication key (a `.p8` file from the Apple developer
    /// account). Unset: no gateway here.
    #[serde(default)]
    pub apns_key_file: Option<PathBuf>,
    #[serde(default)]
    pub apns_key_id: Option<String>,
    #[serde(default)]
    pub apns_team_id: Option<String>,
    /// The app's bundle ID, e.g. `com.example.AudioNet`.
    #[serde(default)]
    pub apns_topic: Option<String>,
    /// Test only: send to this address instead of Apple's (plain HTTP/2).
    #[serde(default)]
    pub apns_url_override: Option<String>,
}

impl PushGatewayConfig {
    pub fn enabled(&self) -> bool {
        self.apns_key_file.is_some()
    }

    fn validate(&self) -> Result<(), String> {
        if !self.enabled() {
            return Ok(());
        }
        for (value, name) in [
            (&self.apns_key_id, "apns_key_id"),
            (&self.apns_team_id, "apns_team_id"),
            (&self.apns_topic, "apns_topic"),
        ] {
            if value.as_deref().is_none_or(str::is_empty) {
                return Err(format!(
                    "push_gateway.apns_key_file is set but push_gateway.{name} is not"
                ));
            }
        }
        Ok(())
    }
}

/// How the connection to the mail server is protected.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
    /// Plain connection upgraded with STARTTLS, which is required (port 587).
    #[default]
    Starttls,
    /// TLS from the start (port 465).
    Tls,
    /// No encryption: only for a mail server on this machine.
    None,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailConfig {
    /// Sender, e.g. `AudioNet <no-reply@audionet.example.com>`. Without it
    /// (and `smtp_host`) the server sends no email and offers no password
    /// reset; accounts still keep their addresses.
    #[serde(default)]
    pub from: Option<String>,
    /// Mail server to send through, e.g. `smtp.example.com`.
    #[serde(default)]
    pub smtp_host: Option<String>,
    /// Defaults to 587 for STARTTLS, 465 for TLS and 25 for none.
    #[serde(default)]
    pub smtp_port: Option<u16>,
    #[serde(default)]
    pub smtp_security: SmtpSecurity,
    #[serde(default)]
    pub smtp_username: Option<String>,
    /// Prefer the `AUDIONET_SMTP_PASSWORD` environment variable.
    #[serde(default)]
    pub smtp_password: Option<String>,
}

impl std::fmt::Debug for EmailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailConfig")
            .field("from", &self.from)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_security", &self.smtp_security)
            .field("smtp_username", &self.smtp_username)
            .field(
                "smtp_password",
                &self.smtp_password.as_ref().map(|_| "(set)"),
            )
            .finish()
    }
}

impl EmailConfig {
    pub fn port(&self) -> u16 {
        self.smtp_port.unwrap_or(match self.smtp_security {
            SmtpSecurity::Starttls => 587,
            SmtpSecurity::Tls => 465,
            SmtpSecurity::None => 25,
        })
    }

    fn validate(&self) -> Result<(), String> {
        match (&self.from, &self.smtp_host) {
            (None, None) => return Ok(()),
            (Some(_), None) => return Err("email.from is set but email.smtp_host is not".into()),
            (None, Some(_)) => return Err("email.smtp_host is set but email.from is not".into()),
            (Some(_), Some(_)) => {}
        }
        let host = self.smtp_host.as_deref().unwrap_or_default();
        if self.smtp_security == SmtpSecurity::None
            && !matches!(host, "localhost" | "127.0.0.1" | "::1")
        {
            return Err(
                "email.smtp_security = \"none\" is only allowed for a mail server on this machine (localhost)"
                    .into(),
            );
        }
        if self.smtp_username.is_some() && self.smtp_password.as_deref().is_none_or(str::is_empty) {
            return Err(
                "email.smtp_username is set but no password is (set AUDIONET_SMTP_PASSWORD)".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IceConfig {
    /// STUN servers, e.g. `stun:audionet.example.com:3478`.
    #[serde(default)]
    pub stun_urls: Vec<String>,
    /// TURN servers, e.g. `turn:audionet.example.com:3478?transport=udp`.
    #[serde(default)]
    pub turn_urls: Vec<String>,
    /// Shared secret with the TURN server (coturn `static-auth-secret`).
    /// Prefer the `AUDIONET_TURN_SECRET` environment variable.
    #[serde(default)]
    pub turn_secret: Option<String>,
    /// Lifetime of generated TURN credentials.
    #[serde(default = "default_turn_ttl")]
    pub turn_credential_ttl_s: u64,
}

fn default_bind() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8740))
}
fn default_database() -> PathBuf {
    PathBuf::from("audionet.db")
}
fn default_session_days() -> u32 {
    30
}
fn default_sign_ups_per_address() -> u32 {
    3
}
fn default_sign_ups_total() -> u32 {
    30
}
fn default_turn_ttl() -> u64 {
    3600
}

impl Config {
    /// Reads `path`, then applies `AUDIONET_*` environment overrides, and
    /// validates everything needed to serve.
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut config = Self::load_unchecked(path)?;
        config.validate()?;
        Ok(config)
    }

    /// Like [`load`](Self::load) but only checks what administrative
    /// commands (user management) need: secrets that only the running
    /// service has (the TURN secret, the mail password) may be absent.
    pub fn load_for_admin(path: &Path) -> Result<Self, String> {
        let mut config = Self::load_unchecked(path)?;
        let turn = std::mem::take(&mut config.ice.turn_urls);
        let email = std::mem::take(&mut config.email);
        config.validate()?;
        config.ice.turn_urls = turn;
        config.email = email;
        Ok(config)
    }

    fn load_unchecked(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let mut config = Self::parse(&text)?;
        config.apply_env(|k| std::env::var(k).ok())?;
        Ok(config)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| format!("invalid configuration: {e}"))
    }

    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) -> Result<(), String> {
        if let Some(v) = get("AUDIONET_PUBLIC_URL") {
            self.public_url = v;
        }
        if let Some(v) = get("AUDIONET_BIND") {
            self.bind = v
                .parse()
                .map_err(|_| format!("AUDIONET_BIND is not an address and port: {v}"))?;
        }
        if let Some(v) = get("AUDIONET_DATABASE") {
            self.database = v.into();
        }
        if let Some(v) = get("AUDIONET_WEB_ROOT") {
            self.web_root = Some(v.into());
        }
        if let Some(v) = get("AUDIONET_DOWNLOADS_DIR") {
            self.downloads_dir = Some(v.into());
        }
        if let Some(v) = get("AUDIONET_ALLOWED_ORIGINS") {
            self.allowed_origins = v
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Some(v) = get("AUDIONET_TURN_SECRET") {
            self.ice.turn_secret = Some(v);
        }
        if let Some(v) = get("AUDIONET_SMTP_PASSWORD") {
            self.email.smtp_password = Some(v);
        }
        Ok(())
    }

    pub fn validate(&mut self) -> Result<(), String> {
        let origin = origin_of(&self.public_url).ok_or_else(|| {
            format!(
                "public_url must look like https://host[:port]: {}",
                self.public_url
            )
        })?;
        if self.allowed_origins.is_empty() {
            self.allowed_origins.push(origin);
        }
        if !self.ice.turn_urls.is_empty()
            && self.ice.turn_secret.as_deref().is_none_or(str::is_empty)
        {
            return Err(
                "turn_urls are set but no TURN secret is configured (set AUDIONET_TURN_SECRET)"
                    .into(),
            );
        }
        self.email.validate()?;
        self.push_gateway.validate()?;
        if let Some(url) = &self.push_gateway_url {
            if origin_of(url).is_none() {
                return Err(format!(
                    "push_gateway_url must look like https://host[:port]: {url}"
                ));
            }
        }
        Ok(())
    }

    /// Whether cookies must be marked `Secure`.
    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

/// `scheme://host[:port]` of a URL, or `None` if it is not http(s).
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() || host.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{}", host.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_and_defaults() {
        let mut c = Config::parse(r#"public_url = "https://audionet.example.com""#).unwrap();
        c.validate().unwrap();
        assert_eq!(c.bind, default_bind());
        assert_eq!(c.allowed_origins, ["https://audionet.example.com"]);
        assert!(c.secure_cookies());
        assert!(!c.allow_registration);
        assert_eq!(c.sign_ups_per_address_per_hour, 3);
        assert_eq!(c.sign_ups_per_hour, 30);
        assert_eq!(c.client_address_header, None);
    }

    #[test]
    fn env_overrides_secrets() {
        let mut c = Config::parse(
            r#"
            public_url = "https://audio.example.org/"
            [ice]
            turn_urls = ["turn:audio.example.org:3478"]
            "#,
        )
        .unwrap();
        assert!(
            c.clone().validate().is_err(),
            "TURN without a secret is rejected"
        );
        c.apply_env(|k| (k == "AUDIONET_TURN_SECRET").then(|| "s3cret".into()))
            .unwrap();
        c.validate().unwrap();
        assert_eq!(c.ice.turn_secret.as_deref(), Some("s3cret"));
    }

    #[test]
    fn email_settings() {
        let parse = |extra: &str| {
            let mut c =
                Config::parse(&format!("public_url = \"https://a.example.com\"\n{extra}")).unwrap();
            c.validate().map(|()| c)
        };
        let c = parse("").unwrap();
        assert!(c.email.from.is_none());
        let c = parse(
            "[email]\nfrom = \"AudioNet <no-reply@example.com>\"\nsmtp_host = \"smtp.example.com\"",
        )
        .unwrap();
        assert_eq!(c.email.port(), 587);
        assert!(
            parse("[email]\nfrom = \"x@example.com\"").is_err(),
            "no host"
        );
        assert!(
            parse("[email]\nfrom = \"x@example.com\"\nsmtp_host = \"smtp.example.com\"\nsmtp_security = \"none\"").is_err(),
            "unencrypted only to this machine"
        );
        assert!(parse("[email]\nfrom = \"x@example.com\"\nsmtp_host = \"localhost\"\nsmtp_security = \"none\"").is_ok());
        let mut c = Config::parse(
            "public_url = \"https://a.example.com\"\n[email]\nfrom = \"x@example.com\"\nsmtp_host = \"smtp.example.com\"\nsmtp_security = \"tls\"\nsmtp_username = \"u\"",
        )
        .unwrap();
        assert!(c.clone().validate().is_err(), "username without a password");
        c.apply_env(|k| (k == "AUDIONET_SMTP_PASSWORD").then(|| "pw".into()))
            .unwrap();
        c.validate().unwrap();
        assert_eq!(c.email.port(), 465);
        assert!(
            !format!("{c:?}").contains("pw\""),
            "the password is not printed"
        );
    }

    #[test]
    fn admin_commands_need_no_service_secrets() {
        let dir = std::env::temp_dir().join(format!("audionet-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            concat!(
                "public_url = \"https://a.example.com\"\n",
                "[ice]\n",
                "turn_urls = [\"turn:a.example.com:3478\"]\n",
                "[email]\n",
                "from = \"x@example.com\"\n",
                "smtp_host = \"smtp.example.com\"\n",
                "smtp_username = \"u\"\n",
            ),
        )
        .unwrap();
        // Unless the test environment happens to set them.
        if std::env::var_os("AUDIONET_TURN_SECRET").is_none()
            && std::env::var_os("AUDIONET_SMTP_PASSWORD").is_none()
        {
            assert!(Config::load(&path).is_err(), "serving needs the secrets");
            let c = Config::load_for_admin(&path).unwrap();
            assert_eq!(c.email.smtp_username.as_deref(), Some("u"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn origins() {
        assert_eq!(
            origin_of("https://A.example.com:8443/x"),
            Some("https://a.example.com:8443".into())
        );
        assert_eq!(origin_of("ftp://x"), None);
        assert_eq!(origin_of("https://"), None);
        assert!(Config::parse("public_url = \"x\"\nunknown = 1").is_err());
    }
}
