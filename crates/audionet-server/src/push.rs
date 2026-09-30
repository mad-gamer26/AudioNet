//! Push notifications about an account's devices, for phones that asked
//! (pushers): a device coming online or going offline, and starting or
//! stopping sharing its audio. Off unless the phone turns them on.
//!
//! What is worth a notification:
//! * Offline: only after the device has stayed away [`PushTiming::offline_grace`]
//!   (a short network blip or a restart of the app says nothing). Online:
//!   only after an offline notification, or when it was away longer than
//!   that; not while devices reconnect after this server starts.
//! * Sharing: a change the device reports while online (not its report on
//!   connecting, and not the first one this server hears).
//!
//! The text (title: the account name; body: "Studio PC is online.") is
//! encrypted for the phone with ChaCha20-Poly1305 and the phone's own key;
//! the push gateway and Apple see only ciphertext. The server sends only to
//! the gateway in its configuration.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::Deserialize;
use serde_json::json;

use crate::api::{ApiError, Principal, Shared};
use crate::db;
use crate::http_client::{self, PlainHttp};
use crate::hub::Presence;

/// How long to wait before saying a device is offline, and how long after
/// this server starts devices' reconnecting says nothing.
#[derive(Clone, Copy, Debug)]
pub struct PushTiming {
    pub offline_grace: Duration,
    pub startup_quiet: Duration,
}

impl Default for PushTiming {
    fn default() -> Self {
        Self {
            offline_grace: Duration::from_secs(30),
            startup_quiet: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Presence,
    Sharing,
}

/// One notification to send to the phones of an account.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Notice {
    user_id: i64,
    node_id: String,
    kind: Kind,
    body: String,
}

/// The decisions, apart from the sending (so they can be tested with a
/// clock of their own).
#[derive(Debug)]
struct Decider {
    timing: PushTiming,
    started: Instant,
    /// Devices gone, and the notice to send if they stay gone.
    pending_offline: HashMap<String, (Instant, Notice)>,
    /// Devices whose offline notice was sent.
    offline_sent: HashSet<String>,
    /// Devices online now, and when they came.
    online_since: HashMap<String, Instant>,
    /// The sharing each device last reported.
    sharing: HashMap<String, bool>,
}

impl Decider {
    fn new(timing: PushTiming, now: Instant) -> Self {
        Self {
            timing,
            started: now,
            pending_offline: HashMap::new(),
            offline_sent: HashSet::new(),
            online_since: HashMap::new(),
            sharing: HashMap::new(),
        }
    }

    fn event(&mut self, p: Presence, now: Instant) -> Option<Notice> {
        match p {
            Presence::Online {
                user_id,
                node_id,
                name,
            } => {
                self.online_since.insert(node_id.clone(), now);
                if self.pending_offline.remove(&node_id).is_some() {
                    return None; // a blip: nothing was said, nothing to say
                }
                let was_announced_offline = self.offline_sent.remove(&node_id);
                let quiet = now.duration_since(self.started) < self.timing.startup_quiet;
                (was_announced_offline || !quiet).then(|| Notice {
                    user_id,
                    body: format!("{name} is online."),
                    node_id,
                    kind: Kind::Presence,
                })
            }
            Presence::Offline {
                user_id,
                node_id,
                name,
            } => {
                self.online_since.remove(&node_id);
                let notice = Notice {
                    user_id,
                    body: format!("{name} is offline."),
                    node_id: node_id.clone(),
                    kind: Kind::Presence,
                };
                self.pending_offline
                    .insert(node_id, (now + self.timing.offline_grace, notice));
                None
            }
            Presence::Sharing {
                user_id,
                node_id,
                name,
                sharing,
            } => {
                let before = self.sharing.insert(node_id.clone(), sharing);
                // Only a change while the device stays online (a device
                // reports its sharing on connecting).
                let settled = self
                    .online_since
                    .get(&node_id)
                    .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(2));
                (before.is_some_and(|b| b != sharing) && settled).then(|| Notice {
                    user_id,
                    body: if sharing {
                        format!("{name} started sharing its audio.")
                    } else {
                        format!("{name} stopped sharing its audio.")
                    },
                    node_id,
                    kind: Kind::Sharing,
                })
            }
        }
    }

    /// Offline notices now due.
    fn due(&mut self, now: Instant) -> Vec<Notice> {
        let due: Vec<String> = self
            .pending_offline
            .iter()
            .filter(|(_, (at, _))| *at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        due.into_iter()
            .filter_map(|id| {
                let (_, notice) = self.pending_offline.remove(&id)?;
                self.offline_sent.insert(id);
                Some(notice)
            })
            .collect()
    }
}

/// Encrypts a notification for a phone: base64 of nonce, ciphertext and tag
/// (the layout CryptoKit's `ChaChaPoly.SealedBox(combined:)` reads).
pub fn seal(key: &[u8], title: &str, body: &str) -> Option<String> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
    let mut nonce = [0u8; 12];
    rand::fill(&mut nonce);
    let plain = json!({ "title": title, "body": body }).to_string();
    let sealed = cipher.encrypt(&Nonce::from(nonce), plain.as_bytes()).ok()?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    Some(STANDARD.encode(out))
}

/// Starts the task that sends notifications, if this server has a gateway
/// configured.
pub fn start(s: Shared) {
    let Some(gateway) = s.config.push_gateway_url.clone() else {
        return;
    };
    let Some(mut rx) = s.hub.watch_presence() else {
        return;
    };
    tokio::spawn(async move {
        let mut decider = Decider::new(s.push_timing, Instant::now());
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            let notices = tokio::select! {
                event = rx.recv() => match event {
                    Some(p) => decider.event(p, Instant::now()).into_iter().collect(),
                    None => return,
                },
                _ = tick.tick() => decider.due(Instant::now()),
            };
            for n in notices {
                let (s, gateway) = (s.clone(), gateway.clone());
                tokio::spawn(async move { deliver(&s, &gateway, n).await });
            }
        }
    });
}

/// Sends one notice to every phone of the account that wants its kind.
async fn deliver(s: &Shared, gateway: &str, n: Notice) {
    let (user_id, except) = (n.user_id, n.node_id.clone());
    let found =
        s.db.call(move |c| {
            let user = db::user_by_id(c, user_id)?;
            Ok((user, db::pushers_for_user(c, user_id, &except)?))
        })
        .await;
    let Ok((Some(user), pushers)) = found else {
        return;
    };
    let url = format!("{}/push/v1/send", gateway.trim_end_matches('/'));
    for p in pushers {
        let wanted = match n.kind {
            Kind::Presence => p.presence,
            Kind::Sharing => p.sharing,
        };
        if !wanted {
            continue;
        }
        let Some(payload) = seal(&p.key, &user.username, &n.body) else {
            continue;
        };
        let collapse_id = match n.kind {
            Kind::Presence => format!("online-{}", n.node_id),
            Kind::Sharing => format!("sharing-{}", n.node_id),
        };
        let body = json!({ "handle": p.handle, "payload": payload, "collapse_id": collapse_id });
        match http_client::post(&url, &[], body.to_string().into_bytes(), PlainHttp::Http1).await {
            Ok(r) if r.status == 200 => {}
            Ok(r) if r.status == 410 => {
                // The phone no longer receives them: forget its pusher.
                let node = p.node_id.clone();
                let _ = s.db.call(move |c| db::delete_pusher(c, &node)).await;
            }
            Ok(r) => tracing::warn!("push gateway answered {}", r.status),
            Err(e) => tracing::warn!("push gateway unreachable: {e}"),
        }
    }
}

// ─── the phone's side: turning notifications on and off ───────────────────

#[derive(Deserialize)]
pub(crate) struct SetPusher {
    /// The gateway the phone registered with; must be this server's.
    gateway: String,
    handle: String,
    /// 32 bytes, base64: the key notifications are encrypted with.
    key: String,
    presence: bool,
    sharing: bool,
}

fn device_only(p: &Principal) -> Result<String, ApiError> {
    p.node.as_ref().map(|n| n.id.clone()).ok_or_else(|| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "not_a_device",
            "Only a signed-in device can receive notifications.",
        )
    })
}

pub(crate) async fn set_pusher(
    State(s): State<Shared>,
    p: Principal,
    Json(r): Json<SetPusher>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let node_id = device_only(&p)?;
    let Some(gateway) = &s.config.push_gateway_url else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_unavailable",
            "This server does not send notifications.",
        ));
    };
    if r.gateway.trim_end_matches('/') != gateway.trim_end_matches('/') {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "wrong_gateway",
            format!("This server sends notifications only through {gateway}."),
        ));
    }
    let key = STANDARD.decode(r.key.trim()).unwrap_or_default();
    if key.len() != 32 || !r.handle.starts_with("anp_") || r.handle.len() > 100 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_pusher",
            "The notification key or handle is not valid.",
        ));
    }
    let pusher = db::Pusher {
        node_id,
        handle: r.handle,
        key,
        presence: r.presence,
        sharing: r.sharing,
    };
    let (presence, sharing) = (pusher.presence, pusher.sharing);
    if presence || sharing {
        s.db.call(move |c| db::set_pusher(c, &pusher)).await?;
    } else {
        let node = pusher.node_id.clone();
        s.db.call(move |c| db::delete_pusher(c, &node)).await?;
    }
    Ok(Json(json!({ "presence": presence, "sharing": sharing })))
}

pub(crate) async fn delete_pusher(
    State(s): State<Shared>,
    p: Principal,
) -> Result<Json<serde_json::Value>, ApiError> {
    let node_id = device_only(&p)?;
    s.db.call(move |c| db::delete_pusher(c, &node_id)).await?;
    Ok(Json(json!({ "presence": false, "sharing": false })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn online(node: &str) -> Presence {
        Presence::Online {
            user_id: 1,
            node_id: node.into(),
            name: node.into(),
        }
    }
    fn offline(node: &str) -> Presence {
        Presence::Offline {
            user_id: 1,
            node_id: node.into(),
            name: node.into(),
        }
    }
    fn sharing(node: &str, on: bool) -> Presence {
        Presence::Sharing {
            user_id: 1,
            node_id: node.into(),
            name: node.into(),
            sharing: on,
        }
    }
    fn bodies(v: Vec<Notice>) -> Vec<String> {
        v.into_iter().map(|n| n.body).collect()
    }

    #[test]
    fn what_is_worth_a_notification() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut d = Decider::new(PushTiming::default(), t0);

        // After a restart, devices reconnecting say nothing.
        assert_eq!(d.event(online("Mac"), s(1)), None);
        assert_eq!(d.event(sharing("Mac", true), s(1)), None, "first report");

        // A blip: gone and back within the grace, nothing at all.
        assert_eq!(d.event(offline("Mac"), s(100)), None);
        assert!(d.due(s(110)).is_empty());
        assert_eq!(d.event(online("Mac"), s(102)), None);
        assert_eq!(
            d.event(sharing("Mac", true), s(102)),
            None,
            "report on connecting"
        );
        assert!(
            d.due(s(200)).is_empty(),
            "the pending offline was cancelled"
        );

        // Really gone: offline after the grace, then online when back.
        d.event(offline("Mac"), s(300));
        assert!(d.due(s(329)).is_empty());
        assert_eq!(bodies(d.due(s(330))), ["Mac is offline."]);
        assert!(d.due(s(400)).is_empty(), "said once");
        let back = d.event(online("Mac"), s(500)).unwrap();
        assert_eq!(back.body, "Mac is online.");
        assert_eq!(back.kind, Kind::Presence);

        // Sharing changes while online.
        assert_eq!(
            d.event(sharing("Mac", false), s(510)).unwrap().body,
            "Mac stopped sharing its audio."
        );
        assert_eq!(d.event(sharing("Mac", false), s(511)), None, "no change");
        let on = d.event(sharing("Mac", true), s(520)).unwrap();
        assert_eq!(
            (on.body.as_str(), on.kind),
            ("Mac started sharing its audio.", Kind::Sharing)
        );

        // A device first seen after the quiet period: online is said.
        assert_eq!(
            d.event(online("Phone"), s(600)).unwrap().body,
            "Phone is online."
        );
    }

    #[test]
    fn sealed_for_the_phone() {
        let key = [7u8; 32];
        let sealed = STANDARD
            .decode(seal(&key, "alice", "Mac is online.").unwrap())
            .unwrap();
        let (nonce, rest) = sealed.split_at(12);
        let nonce: [u8; 12] = nonce.try_into().unwrap();
        let plain = ChaCha20Poly1305::new_from_slice(&key)
            .unwrap()
            .decrypt(&Nonce::from(nonce), rest)
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(
            (v["title"].as_str(), v["body"].as_str()),
            (Some("alice"), Some("Mac is online."))
        );
        assert!(
            seal(&[1u8; 5], "a", "b").is_none(),
            "a key of the wrong size"
        );
    }
}
