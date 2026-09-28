//! Passwords, tokens, sign-in throttling and TURN credentials.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

/// SHA-256 of a token; what the database stores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenHash(pub [u8; 32]);

pub fn hash_token(token: &str) -> TokenHash {
    TokenHash(Sha256::digest(token.as_bytes()).into())
}

/// A new random token with a readable prefix (`ans_` web session, `ann_`
/// node credential), 256 bits of entropy.
pub fn new_token(prefix: &str) -> String {
    let mut b = [0u8; 32];
    rand::fill(&mut b);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(b))
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    rand::fill(&mut salt);
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

/// A fixed hash to verify against when the user does not exist, so login
/// takes the same time either way.
pub fn dummy_hash() -> &'static str {
    use std::sync::OnceLock;
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("not a real password").expect("hashing works"))
}

pub fn validate_username(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > 64 {
        return Err("A username must be 1 to 64 characters long.");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return Err("A username may contain only letters, digits, dots, dashes and underscores.");
    }
    Ok(())
}

/// Names people cannot pick for themselves (an administrator can still
/// create them): they would look like the server's own staff.
const RESERVED_USERNAMES: &[&str] = &[
    "admin",
    "administrator",
    "audionet",
    "help",
    "moderator",
    "official",
    "root",
    "security",
    "staff",
    "support",
    "system",
];

/// Extra rules for accounts people create themselves.
pub fn validate_new_account(username: &str, password: &str) -> Result<(), &'static str> {
    let lower = username.to_ascii_lowercase();
    if RESERVED_USERNAMES.contains(&lower.as_str()) {
        return Err("That username is reserved. Choose another.");
    }
    if password.to_lowercase().contains(&lower) {
        return Err("The password must not contain the username.");
    }
    Ok(())
}

pub fn validate_password(password: &str) -> Result<(), &'static str> {
    if password.chars().count() < 10 {
        return Err("A password must be at least 10 characters long.");
    }
    if password.len() > 1024 {
        return Err("A password must be at most 1024 bytes long.");
    }
    Ok(())
}

/// Exponential backoff for failed sign-ins, per key
/// (username or client address).
#[derive(Debug, Default)]
pub struct Throttle {
    state: Mutex<HashMap<String, (u32, Instant)>>,
}

impl Throttle {
    /// How long `key` must still wait, if at all.
    pub fn wait_time(&self, key: &str) -> Option<Duration> {
        let map = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (_, until) = map.get(key)?;
        until.checked_duration_since(Instant::now())
    }

    pub fn failure(&self, key: &str) {
        let mut map = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 100_000 {
            // Bound memory under attack: forget expired entries.
            let now = Instant::now();
            map.retain(|_, (_, until)| *until > now);
        }
        let entry = map.entry(key.to_owned()).or_insert((0, Instant::now()));
        entry.0 = entry.0.saturating_add(1);
        // No delay for the first 3 failures, then 1, 2, 4 ... up to 300 s.
        let delay = if entry.0 <= 3 {
            0
        } else {
            (1u64 << (entry.0 - 4).min(9)).min(300)
        };
        entry.1 = Instant::now() + Duration::from_secs(delay);
    }

    pub fn success(&self, key: &str) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

/// Limits how many accounts are created: per client address and for the
/// whole server, each over the last hour. Only accounts actually created
/// count (a taken name or a bad password does not).
#[derive(Debug, Default)]
pub struct SignUpLimit {
    state: Mutex<SignUps>,
}

#[derive(Debug, Default)]
struct SignUps {
    by_address: HashMap<String, Vec<Instant>>,
    all: Vec<Instant>,
}

/// Why a new account must wait, and for how long.
#[derive(Debug, PartialEq, Eq)]
pub enum SignUpWait {
    /// This client address created its share recently.
    Address(Duration),
    /// The server as a whole did.
    Server(Duration),
}

const SIGN_UP_WINDOW: Duration = Duration::from_secs(3600);

impl SignUpLimit {
    /// Whether `address` may create an account now.
    pub fn check(
        &self,
        address: &str,
        per_address: u32,
        total: u32,
        now: Instant,
    ) -> Result<(), SignUpWait> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = |t: &Instant| now.saturating_duration_since(*t) < SIGN_UP_WINDOW;
        s.all.retain(fresh);
        s.by_address.retain(|_, times| {
            times.retain(fresh);
            !times.is_empty()
        });
        let wait = |times: &[Instant], limit: u32| {
            (times.len() >= limit as usize).then(|| {
                let oldest = times.iter().min().copied().unwrap_or(now);
                SIGN_UP_WINDOW.saturating_sub(now.saturating_duration_since(oldest))
            })
        };
        if let Some(d) = wait(&s.all, total) {
            return Err(SignUpWait::Server(d));
        }
        if let Some(d) = s
            .by_address
            .get(address)
            .and_then(|times| wait(times, per_address))
        {
            return Err(SignUpWait::Address(d));
        }
        Ok(())
    }

    /// Records an account created from `address`.
    pub fn record(&self, address: &str, now: Instant) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.all.push(now);
        s.by_address
            .entry(address.to_owned())
            .or_default()
            .push(now);
    }
}

/// Time-limited TURN credentials (coturn `use-auth-secret` / TURN REST API):
/// username `expiry:label`, password `base64(HMAC-SHA1(secret, username))`.
pub fn turn_credentials(secret: &str, label: &str, ttl_s: u64, now_s: u64) -> (String, String) {
    let username = format!("{}:{label}", now_s + ttl_s);
    let mut mac =
        Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(username.as_bytes());
    let credential = STANDARD.encode(mac.finalize().into_bytes());
    (username, credential)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_up_limit_per_address_and_server() {
        let limit = SignUpLimit::default();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(limit.check("a", 3, 5, t0), Ok(()));
            limit.record("a", t0);
        }
        // The address has used its 3; another address has not.
        assert!(matches!(
            limit.check("a", 3, 5, t0),
            Err(SignUpWait::Address(d)) if d == SIGN_UP_WINDOW
        ));
        assert_eq!(limit.check("b", 3, 5, t0), Ok(()));
        limit.record("b", t0);
        limit.record("c", t0 + Duration::from_secs(600));
        // Five in the hour: the server is full for everyone.
        assert!(matches!(
            limit.check("d", 3, 5, t0 + Duration::from_secs(600)),
            Err(SignUpWait::Server(_))
        ));
        // An hour after the first ones, room again.
        let later = t0 + SIGN_UP_WINDOW + Duration::from_secs(1);
        assert_eq!(limit.check("a", 3, 5, later), Ok(()));
    }

    #[test]
    fn new_account_rules() {
        assert!(validate_new_account("Admin", "a long password").is_err());
        assert!(validate_new_account("alice", "ALICE-rocks-2026").is_err());
        assert!(validate_new_account("alice", "correct horse battery").is_ok());
    }

    #[test]
    fn passwords() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not a hash"));
        assert!(validate_password("short").is_err());
        assert!(validate_username("bad name").is_err());
        assert!(validate_username("good.name-1").is_ok());
    }

    #[test]
    fn tokens() {
        let t = new_token("ans_");
        assert!(t.starts_with("ans_") && t.len() > 40);
        assert_ne!(new_token("x"), new_token("x"));
        assert_eq!(hash_token("a"), hash_token("a"));
    }

    #[test]
    fn throttle_backs_off() {
        let t = Throttle::default();
        for _ in 0..3 {
            t.failure("alice");
        }
        assert!(t.wait_time("alice").is_none(), "three free attempts");
        t.failure("alice");
        assert!(t.wait_time("alice").is_some());
        t.success("alice");
        assert!(t.wait_time("alice").is_none());
    }

    #[test]
    fn turn_rest_credentials_match_the_coturn_scheme() {
        // Known answer computed independently with Python:
        // base64(hmac.new(b'secret', b'1700003600:alice', sha1).digest())
        let (u, p) = turn_credentials("secret", "alice", 3600, 1_700_000_000);
        assert_eq!(u, "1700003600:alice");
        assert_eq!(p, "LLPLO4qjdVL2qZhwr3eImhn7J20=");
    }
}
