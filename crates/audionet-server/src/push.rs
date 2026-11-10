//! Push notifications about an account's devices, for phones that asked
//! (pushers): a device becoming visible to the account, and no longer being
//! visible (visibility turned off, or disconnected). Off unless the phone
//! turns them on.
//!
//! What is worth a notification:
//! * No longer visible: only after the device has stayed so for
//!   [`PushTiming::offline_grace`] (a short network blip or a restart of the
//!   app says nothing).
//! * Now visible: only after a "no longer visible" notification, or when it
//!   was away longer than that; not while devices reconnect after this
//!   server starts. It waits [`REPORT_WAIT`], so a device that hides again
//!   at once (as one that predates visibility does when not sharing) is
//!   never announced.
//!
//! The text (title: the account name; body: "Studio PC is now visible.")
//! and what it is about (the device's id and name, the event: `online` or
//! `offline`, and `sharing`: whether the device is visible now, under the
//! names apps that predate visibility read, so the phone can update its
//! widgets and offer to listen) are
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

/// How long a "now visible" notice waits before it is sent: a device that
/// predates visibility connects visible and may report itself not visible
/// (its "not sharing") right after; it is then never announced.
pub const REPORT_WAIT: Duration = Duration::from_secs(2);

/// One notification to send to the phones of an account.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Notice {
    user_id: i64,
    node_id: String,
    body: String,
    /// The device's name, and what happened to it: `online` (it is now
    /// visible) or `offline` (no longer visible). These are the names apps
    /// that predate visibility know.
    device: String,
    event: &'static str,
    /// Whether the device is visible now (`sharing` to those apps).
    visible: bool,
}

/// The decisions, apart from the sending (so they can be tested with a
/// clock of their own).
#[derive(Debug)]
struct Decider {
    timing: PushTiming,
    started: Instant,
    /// Devices no longer visible, and the notice to send if they stay so.
    pending_hidden: HashMap<String, (Instant, Notice)>,
    /// Devices whose "no longer visible" notice was sent.
    hidden_sent: HashSet<String>,
    /// "Now visible" notices waiting out REPORT_WAIT.
    pending_visible: HashMap<String, (Instant, Notice)>,
}

impl Decider {
    fn new(timing: PushTiming, now: Instant) -> Self {
        Self {
            timing,
            started: now,
            pending_hidden: HashMap::new(),
            hidden_sent: HashSet::new(),
            pending_visible: HashMap::new(),
        }
    }

    fn event(&mut self, p: Presence, now: Instant) -> Option<Notice> {
        match p {
            Presence::Visible {
                user_id,
                node_id,
                name,
            } => {
                if self.pending_hidden.remove(&node_id).is_some() {
                    return None; // a blip: nothing was said, nothing to say
                }
                let was_announced_hidden = self.hidden_sent.remove(&node_id);
                let quiet = now.duration_since(self.started) < self.timing.startup_quiet;
                if was_announced_hidden || !quiet {
                    let notice = Notice {
                        user_id,
                        body: format!("{name} is now visible."),
                        node_id: node_id.clone(),
                        device: name,
                        event: "online",
                        visible: true,
                    };
                    self.pending_visible
                        .insert(node_id, (now + REPORT_WAIT, notice));
                }
                None
            }
            Presence::Hidden {
                user_id,
                node_id,
                name,
            } => {
                if self.pending_visible.remove(&node_id).is_some() {
                    // Never announced as visible: say nothing now, and
                    // still announce it when it really becomes visible.
                    self.hidden_sent.insert(node_id);
                    return None;
                }
                let notice = Notice {
                    user_id,
                    body: format!("{name} is no longer visible."),
                    node_id: node_id.clone(),
                    device: name,
                    event: "offline",
                    visible: false,
                };
                self.pending_hidden
                    .insert(node_id, (now + self.timing.offline_grace, notice));
                None
            }
        }
    }

    /// Notices now due: "now visible" after REPORT_WAIT, "no longer
    /// visible" after the grace.
    fn due(&mut self, now: Instant) -> Vec<Notice> {
        let take = |m: &mut HashMap<String, (Instant, Notice)>| {
            let ids: Vec<String> = m
                .iter()
                .filter(|(_, (at, _))| *at <= now)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| m.remove(&id).map(|(_, n)| (id, n)))
                .collect::<Vec<_>>()
        };
        let mut out: Vec<Notice> = take(&mut self.pending_visible)
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        for (id, n) in take(&mut self.pending_hidden) {
            self.hidden_sent.insert(id);
            out.push(n);
        }
        out
    }
}

/// What a notification is about, sealed with its text.
#[derive(Clone, Copy, Debug)]
pub struct About<'a> {
    pub node_id: &'a str,
    pub device: &'a str,
    pub event: &'a str,
    /// Whether the device is visible now.
    pub visible: bool,
}

/// Encrypts a notification for a phone: base64 of nonce, ciphertext and tag
/// (the layout CryptoKit's `ChaChaPoly.SealedBox(combined:)` reads).
///
/// Every value is a string: the iPhone app's notification extension reads
/// the object as strings only, so `sharing` (whether the device is visible
/// now, under the name apps before visibility read) is "true" or "false".
pub fn seal(key: &[u8], title: &str, body: &str, about: About<'_>) -> Option<String> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
    let mut nonce = [0u8; 12];
    rand::fill(&mut nonce);
    let plain = json!({
        "title": title,
        "body": body,
        "node_id": about.node_id,
        "device": about.device,
        "event": about.event,
        "sharing": if about.visible { "true" } else { "false" },
    })
    .to_string();
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
        // One kind of notification now (visibility); either of the two
        // choices phones that predate it made means they want it.
        if !(p.presence || p.sharing) {
            continue;
        }
        let about = About {
            node_id: &n.node_id,
            device: &n.device,
            event: n.event,
            visible: n.visible,
        };
        let Some(payload) = seal(&p.key, &user.username, &n.body, about) else {
            continue;
        };
        let collapse_id = format!("online-{}", n.node_id);
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

    fn visible(node: &str) -> Presence {
        Presence::Visible {
            user_id: 1,
            node_id: node.into(),
            name: node.into(),
        }
    }
    fn hidden(node: &str) -> Presence {
        Presence::Hidden {
            user_id: 1,
            node_id: node.into(),
            name: node.into(),
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
        assert_eq!(d.event(visible("Mac"), s(1)), None);
        assert!(d.due(s(50)).is_empty());

        // A blip: gone and back within the grace, nothing at all.
        assert_eq!(d.event(hidden("Mac"), s(100)), None);
        assert!(d.due(s(110)).is_empty());
        assert_eq!(d.event(visible("Mac"), s(102)), None);
        assert!(d.due(s(200)).is_empty(), "the pending one was cancelled");

        // Really gone: said after the grace, then said again when back
        // (after REPORT_WAIT).
        d.event(hidden("Mac"), s(300));
        assert!(d.due(s(329)).is_empty());
        let gone = d.due(s(330));
        assert_eq!(bodies(gone.clone()), ["Mac is no longer visible."]);
        assert_eq!((gone[0].event, gone[0].visible), ("offline", false));
        assert!(d.due(s(400)).is_empty(), "said once");
        assert_eq!(d.event(visible("Mac"), s(500)), None);
        assert!(d.due(s(501)).is_empty());
        let back = d.due(s(502));
        assert_eq!(bodies(back.clone()), ["Mac is now visible."]);
        assert_eq!((back[0].event, back[0].visible), ("online", true));

        // A device first seen after the quiet period is announced.
        assert_eq!(d.event(visible("Phone"), s(600)), None);
        assert_eq!(bodies(d.due(s(602))), ["Phone is now visible."]);

        // One that hides again at once (an older app not sharing) is never
        // announced, and is announced when it really becomes visible.
        assert_eq!(d.event(visible("Old PC"), s(700)), None);
        assert_eq!(d.event(hidden("Old PC"), s(700)), None);
        assert!(d.due(s(800)).is_empty(), "nothing about it");
        assert_eq!(d.event(visible("Old PC"), s(900)), None);
        assert_eq!(bodies(d.due(s(902))), ["Old PC is now visible."]);
    }

    const MAC_ONLINE: About<'static> = About {
        node_id: "node_mac",
        device: "Mac",
        event: "online",
        visible: true,
    };

    #[test]
    fn sealed_for_the_phone() {
        let key = [7u8; 32];
        let sealed = STANDARD
            .decode(seal(&key, "alice", "Mac is online.", MAC_ONLINE).unwrap())
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
        assert_eq!(
            (
                v["node_id"].as_str(),
                v["device"].as_str(),
                v["event"].as_str()
            ),
            (Some("node_mac"), Some("Mac"), Some("online"))
        );
        assert_eq!(
            v["sharing"].as_str(),
            Some("true"),
            "visible, as a string, as the phone reads it"
        );
        assert!(
            v.as_object().unwrap().values().all(|x| x.is_string()),
            "every value a string"
        );
        assert!(
            seal(&[1u8; 5], "a", "b", MAC_ONLINE).is_none(),
            "a key of the wrong size"
        );
    }
}
