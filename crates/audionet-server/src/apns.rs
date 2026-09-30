//! Sending to Apple's push service (APNs) with a token-based key: an ES256
//! JSON Web Token signed with the `.p8` key, renewed before Apple's one-hour
//! limit, over HTTP/2. Only a push gateway has a key.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};

use crate::config::PushGatewayConfig;
use crate::http_client::{self, PlainHttp};

/// Apple accepts a token for an hour; a fresh one well before that.
const TOKEN_LIFETIME: Duration = Duration::from_secs(40 * 60);

/// What Apple said about one notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// The device token is no longer valid (the app was removed, or the
    /// token belongs elsewhere): forget it.
    Unregistered,
    Failed(String),
}

/// A notification as the in-memory sender records it (tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub device_token: String,
    pub sandbox: bool,
    pub collapse_id: String,
    pub payload: serde_json::Value,
}

pub enum Apns {
    Real(Box<Client>),
    /// Records instead of sending; device tokens starting `dead` answer
    /// "unregistered" (tests).
    Memory(Arc<Mutex<Vec<Sent>>>),
}

impl std::fmt::Debug for Apns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Real(_) => "Apns::Real",
            Self::Memory(_) => "Apns::Memory",
        })
    }
}

pub struct Client {
    key: EcdsaKeyPair,
    key_id: String,
    team_id: String,
    topic: String,
    url_override: Option<String>,
    token: Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("key_id", &self.key_id)
            .field("team_id", &self.team_id)
            .field("topic", &self.topic)
            .finish_non_exhaustive()
    }
}

/// The DER bytes of a PEM file (the `.p8` key).
fn pem_der(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .concat();
    STANDARD
        .decode(body.trim())
        .map_err(|e| format!("the APNs key is not a PEM file: {e}"))
}

impl Apns {
    /// The sender a gateway configuration describes; `None` if this server
    /// runs no gateway.
    pub fn from_config(c: &PushGatewayConfig) -> Result<Option<Self>, String> {
        let Some(path) = &c.apns_key_file else {
            return Ok(None);
        };
        let pem = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read the APNs key {}: {e}", path.display()))?;
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            &pem_der(&pem)?,
            &SystemRandom::new(),
        )
        .map_err(|e| format!("the APNs key {} is not a P-256 key: {e}", path.display()))?;
        Ok(Some(Self::Real(Box::new(Client {
            key,
            key_id: c.apns_key_id.clone().unwrap_or_default(),
            team_id: c.apns_team_id.clone().unwrap_or_default(),
            topic: c.apns_topic.clone().unwrap_or_default(),
            url_override: c.apns_url_override.clone(),
            token: Mutex::new(None),
        }))))
    }

    pub fn memory() -> (Self, Arc<Mutex<Vec<Sent>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (Self::Memory(Arc::clone(&sent)), sent)
    }

    /// Sends an alert notification to one device.
    pub async fn send(
        &self,
        device_token: &str,
        sandbox: bool,
        collapse_id: &str,
        payload: serde_json::Value,
    ) -> Outcome {
        match self {
            Self::Memory(sent) => {
                if device_token.starts_with("dead") {
                    return Outcome::Unregistered;
                }
                sent.lock().unwrap_or_else(|e| e.into_inner()).push(Sent {
                    device_token: device_token.to_owned(),
                    sandbox,
                    collapse_id: collapse_id.to_owned(),
                    payload,
                });
                Outcome::Sent
            }
            Self::Real(c) => c.send(device_token, sandbox, collapse_id, payload).await,
        }
    }
}

impl Client {
    /// The current provider token (ES256 JWT), renewed when due.
    fn provider_token(&self) -> Result<String, String> {
        let mut cached = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((token, at)) = cached.as_ref() {
            if at.elapsed() < TOKEN_LIFETIME {
                return Ok(token.clone());
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let header = URL_SAFE_NO_PAD
            .encode(serde_json::json!({ "alg": "ES256", "kid": self.key_id }).to_string());
        let claims = URL_SAFE_NO_PAD
            .encode(serde_json::json!({ "iss": self.team_id, "iat": now }).to_string());
        let signing_input = format!("{header}.{claims}");
        let signature = self
            .key
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| "could not sign the APNs token".to_owned())?;
        let token = format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        );
        *cached = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    async fn send(
        &self,
        device_token: &str,
        sandbox: bool,
        collapse_id: &str,
        payload: serde_json::Value,
    ) -> Outcome {
        let token = match self.provider_token() {
            Ok(t) => t,
            Err(e) => return Outcome::Failed(e),
        };
        let base = self.url_override.clone().unwrap_or_else(|| {
            if sandbox {
                "https://api.sandbox.push.apple.com".into()
            } else {
                "https://api.push.apple.com".into()
            }
        });
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + 3600;
        let headers = [
            ("authorization", format!("bearer {token}")),
            ("apns-topic", self.topic.clone()),
            ("apns-push-type", "alert".to_owned()),
            ("apns-priority", "10".to_owned()),
            ("apns-expiration", expires.to_string()),
            ("apns-collapse-id", collapse_id.to_owned()),
        ];
        let url = format!("{}/3/device/{device_token}", base.trim_end_matches('/'));
        match http_client::post(
            &url,
            &headers,
            payload.to_string().into_bytes(),
            PlainHttp::Http2,
        )
        .await
        {
            Ok(r) if r.status == 200 => Outcome::Sent,
            Ok(r) => {
                let reason = serde_json::from_slice::<serde_json::Value>(&r.body)
                    .ok()
                    .and_then(|v| v["reason"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                if r.status == 410
                    || matches!(
                        reason.as_str(),
                        "BadDeviceToken" | "Unregistered" | "DeviceTokenNotForTopic"
                    )
                {
                    Outcome::Unregistered
                } else {
                    Outcome::Failed(format!("Apple answered {} {reason}", r.status))
                }
            }
            Err(e) => Outcome::Failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    #[test]
    fn provider_tokens_are_es256_jwts() {
        // A throwaway P-256 key in PKCS#8, as Apple's .p8 files are.
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(pkcs8.as_ref())
        );
        let dir = std::env::temp_dir().join(format!("audionet-apns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key.p8");
        std::fs::write(&path, pem).unwrap();
        let apns = Apns::from_config(&PushGatewayConfig {
            apns_key_file: Some(path),
            apns_key_id: Some("KEY123".into()),
            apns_team_id: Some("TEAM45".into()),
            apns_topic: Some("com.example.AudioNet".into()),
            apns_url_override: None,
        })
        .unwrap()
        .unwrap();
        let Apns::Real(client) = apns else {
            panic!("a real client")
        };
        let token = client.provider_token().unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "KEY123");
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "TEAM45");
        // The signature verifies with the key's public half.
        let public = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_FIXED,
            client.key.public_key().as_ref().to_vec(),
        );
        public
            .verify(
                format!("{}.{}", parts[0], parts[1]).as_bytes(),
                &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
            )
            .unwrap();
        assert_eq!(
            client.provider_token().unwrap(),
            token,
            "reused while fresh"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
