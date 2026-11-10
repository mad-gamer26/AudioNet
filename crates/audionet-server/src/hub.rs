//! The signaling hub: who is connected, which nodes are visible with which
//! endpoints, and which sessions route between which connections.
//!
//! A device is *visible* when it is connected with visibility on: it is
//! listed to its account's other connections, which may listen to it and
//! send to it. A device that is not visible is a visitor, like a browser:
//! listed nowhere and reachable by no one, but it may still listen to and
//! send to the account's visible devices.
//!
//! Every connection has a *bounded* outgoing queue. If a client cannot keep
//! up, it is disconnected rather than allowed to delay other clients
//! (AGENTS.md §36). All state changes happen under one short, non-async lock.

use std::collections::HashMap;
use std::sync::Mutex;

use audionet_protocol::signal::{
    DestinationInfo, NodeSummary, ServerMessage, SessionMedia, SessionState, SourceInfo,
};
use audionet_protocol::{NodeId, Platform, SessionId};
use tokio::sync::mpsc;

/// Outgoing messages queued per connection before it is considered stuck.
pub const QUEUE_DEPTH: usize = 128;

#[derive(Debug)]
struct Conn {
    user_id: i64,
    node: Option<NodeMeta>,
    /// Nodes: whether the device is visible (from its hello; devices that
    /// predate visibility start visible and say "sharing" to change it).
    visible: bool,
    tx: mpsc::Sender<ServerMessage>,
}

#[derive(Clone, Debug)]
pub struct NodeMeta {
    pub id: String,
    pub name: String,
    pub platform: Option<Platform>,
}

#[derive(Debug)]
struct Route {
    offerer: String,
    node_conn: String,
}

#[derive(Debug, Default)]
struct Inner {
    conns: HashMap<String, Conn>,
    node_conn: HashMap<String, String>,
    endpoints: HashMap<String, (Vec<SourceInfo>, Vec<DestinationInfo>)>,
    sessions: HashMap<String, Route>,
}

/// Errors reported back to the requesting client, in words.
#[derive(Debug, PartialEq, Eq)]
pub struct HubError {
    pub code: &'static str,
    pub message: String,
}

fn herr(code: &'static str, message: impl Into<String>) -> HubError {
    HubError {
        code,
        message: message.into(),
    }
}

/// A device became visible to its account, or stopped being visible
/// (visibility turned off, or disconnected), for push notifications; the
/// push task decides what is worth a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    Visible {
        user_id: i64,
        node_id: String,
        name: String,
    },
    Hidden {
        user_id: i64,
        node_id: String,
        name: String,
    },
}

#[derive(Debug, Default)]
pub struct Hub {
    inner: Mutex<Inner>,
    presence: std::sync::OnceLock<mpsc::UnboundedSender<Presence>>,
}

impl Hub {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Device presence changes from now on (for push notifications). Only
    /// the first caller gets them.
    pub fn watch_presence(&self) -> Option<mpsc::UnboundedReceiver<Presence>> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.presence.set(tx).ok().map(|()| rx)
    }

    fn tell_presence(&self, p: Presence) {
        if let Some(tx) = self.presence.get() {
            let _ = tx.send(p);
        }
    }

    /// Registers a connection. For a node, replaces any older connection of
    /// the same node (whose sessions end); `visible` is its visibility from
    /// the start (ignored for browsers).
    pub fn register(
        &self,
        conn_id: &str,
        user_id: i64,
        node: Option<NodeMeta>,
        visible: bool,
        tx: mpsc::Sender<ServerMessage>,
    ) {
        let old = node
            .as_ref()
            .and_then(|n| self.lock().node_conn.get(&n.id).cloned());
        if let Some(old) = old {
            self.unregister(&old);
        }
        let mut g = self.lock();
        if let Some(n) = &node {
            g.node_conn.insert(n.id.clone(), conn_id.to_owned());
            g.endpoints.insert(n.id.clone(), (vec![], vec![]));
        }
        g.conns.insert(
            conn_id.to_owned(),
            Conn {
                user_id,
                node,
                visible,
                tx,
            },
        );
        if let Some(meta) = g.conns.get(conn_id).and_then(|c| c.node.clone())
            && visible
        {
            Self::broadcast_node(&mut g, user_id, &meta);
            drop(g);
            self.tell_presence(Presence::Visible {
                user_id,
                node_id: meta.id,
                name: meta.name,
            });
        }
    }

    /// Removes a connection, ends its sessions, and reports a visible node
    /// no longer visible.
    pub fn unregister(&self, conn_id: &str) {
        let mut g = self.lock();
        let Some(conn) = g.conns.remove(conn_id) else {
            return;
        };
        if let Some(n) = conn.node.as_ref().filter(|_| conn.visible) {
            self.tell_presence(Presence::Hidden {
                user_id: conn.user_id,
                node_id: n.id.clone(),
                name: n.name.clone(),
            });
        }
        let ended: Vec<(String, String)> = g
            .sessions
            .iter()
            .filter(|(_, r)| r.offerer == conn_id || r.node_conn == conn_id)
            .map(|(id, r)| {
                let other = if r.offerer == conn_id {
                    &r.node_conn
                } else {
                    &r.offerer
                };
                (id.clone(), other.clone())
            })
            .collect();
        for (sid, other) in ended {
            g.sessions.remove(&sid);
            if let Ok(session_id) = SessionId::new(sid) {
                Self::send(
                    &mut g,
                    &other,
                    ServerMessage::SessionEnd {
                        session_id,
                        reason: "The other side disconnected.".into(),
                    },
                );
            }
        }
        if let Some(meta) = conn.node
            && g.node_conn.get(&meta.id).is_some_and(|c| c == conn_id)
        {
            g.node_conn.remove(&meta.id);
            g.endpoints.remove(&meta.id);
            if conn.visible {
                Self::broadcast_node(&mut g, conn.user_id, &meta);
            }
        }
    }

    /// Queues a message; disconnects a client whose queue is full.
    fn send(g: &mut Inner, conn_id: &str, msg: ServerMessage) {
        let full = match g.conns.get(conn_id) {
            Some(c) => matches!(c.tx.try_send(msg), Err(mpsc::error::TrySendError::Full(_))),
            None => false,
        };
        if full {
            tracing::warn!(conn = conn_id, "client is not keeping up; disconnecting it");
            // Dropping the sender ends the connection's writer task.
            g.conns.remove(conn_id);
        }
    }

    pub fn send_to(&self, conn_id: &str, msg: ServerMessage) {
        Self::send(&mut self.lock(), conn_id, msg);
    }

    /// Whether a node is connected with visibility on.
    fn is_visible(g: &Inner, node_id: &str) -> bool {
        g.node_conn
            .get(node_id)
            .and_then(|c| g.conns.get(c))
            .is_some_and(|c| c.visible)
    }

    /// A node as its account sees it: `online` and `sharing` both say
    /// whether it is visible (the second for apps that predate visibility).
    fn summary(g: &Inner, meta: &NodeMeta) -> NodeSummary {
        let visible = Self::is_visible(g, &meta.id);
        let (sources, destinations) = g.endpoints.get(&meta.id).cloned().unwrap_or_default();
        NodeSummary {
            node_id: NodeId::new(meta.id.clone())
                .unwrap_or_else(|_| NodeId::new("invalid").expect("valid")),
            name: meta.name.clone(),
            platform: meta.platform,
            online: visible,
            sharing: visible,
            sources,
            destinations,
        }
    }

    /// Tells the owner's other connections (browsers and native apps) about
    /// a node change.
    fn broadcast_node(g: &mut Inner, user_id: i64, meta: &NodeMeta) {
        let summary = Self::summary(g, meta);
        let targets: Vec<String> = g
            .conns
            .iter()
            .filter(|(_, c)| {
                c.user_id == user_id && c.node.as_ref().is_none_or(|n| n.id != meta.id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for t in targets {
            Self::send(
                g,
                &t,
                ServerMessage::NodeUpdate {
                    node: summary.clone(),
                },
            );
        }
    }

    /// A node was renamed: its own connection keeps the new name, and the
    /// owner's other connections are told.
    pub fn node_renamed(&self, user_id: i64, meta: NodeMeta) {
        let mut g = self.lock();
        for c in g.conns.values_mut() {
            if let Some(n) = c.node.as_mut().filter(|n| n.id == meta.id) {
                n.name.clone_from(&meta.name);
            }
        }
        if Self::is_visible(&g, &meta.id) {
            Self::broadcast_node(&mut g, user_id, &meta);
        }
    }

    /// Records a node's current sources and destinations.
    pub fn set_endpoints(
        &self,
        conn_id: &str,
        sources: Vec<SourceInfo>,
        destinations: Vec<DestinationInfo>,
    ) -> Result<(), HubError> {
        let mut g = self.lock();
        let conn = g
            .conns
            .get(conn_id)
            .ok_or_else(|| herr("gone", "Connection closed."))?;
        let meta = conn
            .node
            .clone()
            .ok_or_else(|| herr("not_a_node", "Only devices can report endpoints."))?;
        let user_id = conn.user_id;
        let visible = conn.visible;
        g.endpoints.insert(meta.id.clone(), (sources, destinations));
        if visible {
            Self::broadcast_node(&mut g, user_id, &meta);
        }
        Ok(())
    }

    /// A node turns its visibility on or off. Off, it becomes a visitor:
    /// every session in which another connection reaches it (listening to
    /// it, or sending to it) ends, telling both sides; the sessions it
    /// started itself go on, as a visitor's do.
    pub fn set_visible(&self, conn_id: &str, visible: bool) -> Result<(), HubError> {
        let mut g = self.lock();
        let conn = g
            .conns
            .get_mut(conn_id)
            .ok_or_else(|| herr("gone", "Connection closed."))?;
        let meta = conn
            .node
            .clone()
            .ok_or_else(|| herr("not_a_node", "Only devices have a visibility."))?;
        let user_id = conn.user_id;
        if conn.visible == visible {
            return Ok(());
        }
        conn.visible = visible;
        if !visible {
            let ended: Vec<(String, String, String)> = g
                .sessions
                .iter()
                .filter(|(_, r)| r.node_conn == conn_id)
                .map(|(id, r)| (id.clone(), r.offerer.clone(), r.node_conn.clone()))
                .collect();
            let reason = format!("\"{}\" is no longer visible.", meta.name);
            for (sid, offerer, node_conn) in ended {
                g.sessions.remove(&sid);
                if let Ok(session_id) = SessionId::new(sid) {
                    for to in [offerer, node_conn] {
                        Self::send(
                            &mut g,
                            &to,
                            ServerMessage::SessionEnd {
                                session_id: session_id.clone(),
                                reason: reason.clone(),
                            },
                        );
                    }
                }
            }
        }
        Self::broadcast_node(&mut g, user_id, &meta);
        drop(g);
        self.tell_presence(if visible {
            Presence::Visible {
                user_id,
                node_id: meta.id,
                name: meta.name,
            }
        } else {
            Presence::Hidden {
                user_id,
                node_id: meta.id,
                name: meta.name,
            }
        });
        Ok(())
    }

    /// The visible ones of a user's nodes (given their database rows), as
    /// the account's device list shows them.
    pub fn visible_summaries(&self, nodes: &[NodeMeta]) -> Vec<NodeSummary> {
        let g = self.lock();
        nodes
            .iter()
            .filter(|m| Self::is_visible(&g, &m.id))
            .map(|m| Self::summary(&g, m))
            .collect()
    }

    /// Routes an offer from `from` to the node, if the same user owns it.
    pub fn offer(
        &self,
        from: &str,
        session_id: SessionId,
        node_id: &NodeId,
        media: SessionMedia,
        sdp: String,
    ) -> Result<(), HubError> {
        let mut g = self.lock();
        let user_id = g
            .conns
            .get(from)
            .ok_or_else(|| herr("gone", "Connection closed."))?
            .user_id;
        // A device that is not visible (or not connected, or someone
        // else's) cannot be reached, all with the same answer: this reveals
        // nothing about it.
        let not_visible = || herr("node_offline", "That device is not visible.");
        let node_conn = g
            .node_conn
            .get(node_id.as_str())
            .cloned()
            .ok_or_else(not_visible)?;
        if g.conns
            .get(&node_conn)
            .is_none_or(|c| c.user_id != user_id || !c.visible)
        {
            return Err(not_visible());
        }
        if node_conn == from {
            return Err(herr(
                "invalid",
                "A device cannot start a session with itself.",
            ));
        }
        // Whoever starts a session may listen or send: a visible device, a
        // device that is not visible (a visitor) or a browser.
        if g.sessions.contains_key(session_id.as_str()) {
            return Err(herr(
                "duplicate_session",
                "That session identifier is already in use.",
            ));
        }
        let per_conn = g.sessions.values().filter(|r| r.offerer == from).count();
        if per_conn >= 16 {
            return Err(herr(
                "too_many_sessions",
                "Too many sessions are open on this connection.",
            ));
        }
        g.sessions.insert(
            session_id.as_str().to_owned(),
            Route {
                offerer: from.to_owned(),
                node_conn: node_conn.clone(),
            },
        );
        Self::send(
            &mut g,
            &node_conn,
            ServerMessage::SessionOffer {
                session_id,
                from: from.to_owned(),
                media,
                sdp,
            },
        );
        Ok(())
    }

    /// The other party of a session, if `from` is a party.
    fn peer(g: &Inner, from: &str, session_id: &SessionId) -> Result<(String, bool), HubError> {
        let r = g
            .sessions
            .get(session_id.as_str())
            .ok_or_else(|| herr("no_session", "That session has ended."))?;
        if r.offerer == from {
            Ok((r.node_conn.clone(), false))
        } else if r.node_conn == from {
            Ok((r.offerer.clone(), true))
        } else {
            Err(herr("no_session", "That session has ended."))
        }
    }

    pub fn answer(&self, from: &str, session_id: SessionId, sdp: String) -> Result<(), HubError> {
        let mut g = self.lock();
        let (to, from_node) = Self::peer(&g, from, &session_id)?;
        if !from_node {
            return Err(herr("invalid", "Only the device answers a session."));
        }
        Self::send(
            &mut g,
            &to,
            ServerMessage::SessionAnswer { session_id, sdp },
        );
        Ok(())
    }

    pub fn status(
        &self,
        from: &str,
        session_id: SessionId,
        state: SessionState,
        detail: Option<String>,
    ) -> Result<(), HubError> {
        let mut g = self.lock();
        let (to, _) = Self::peer(&g, from, &session_id)?;
        if matches!(state, SessionState::Ended | SessionState::Failed) {
            g.sessions.remove(session_id.as_str());
        }
        Self::send(
            &mut g,
            &to,
            ServerMessage::SessionStatus {
                session_id,
                state,
                detail,
            },
        );
        Ok(())
    }

    pub fn end(&self, from: &str, session_id: SessionId, reason: String) -> Result<(), HubError> {
        let mut g = self.lock();
        let (to, _) = Self::peer(&g, from, &session_id)?;
        g.sessions.remove(session_id.as_str());
        Self::send(
            &mut g,
            &to,
            ServerMessage::SessionEnd { session_id, reason },
        );
        Ok(())
    }

    /// Disconnects a node (e.g. after it was removed from the account).
    pub fn kick_node(&self, node_id: &str) {
        let conn = self.lock().node_conn.get(node_id).cloned();
        if let Some(c) = conn {
            self.unregister(&c);
        }
    }

    pub fn connection_count(&self) -> usize {
        self.lock().conns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str) -> NodeMeta {
        NodeMeta {
            id: id.into(),
            name: format!("Node {id}"),
            platform: Some(Platform::Windows),
        }
    }

    fn sid(s: &str) -> SessionId {
        SessionId::new(s).unwrap()
    }

    #[test]
    fn routes_offer_answer_end_between_owner_connections() {
        let hub = Hub::default();
        let (btx, mut brx) = mpsc::channel(8);
        let (ntx, mut nrx) = mpsc::channel(8);
        hub.register("b1", 1, None, true, btx);
        hub.register("n1c", 1, Some(meta("n1")), true, ntx);
        // Browser hears that the node came online.
        assert!(matches!(brx.try_recv(), Ok(ServerMessage::NodeUpdate { node }) if node.online));

        let media = SessionMedia::Listen {
            source_id: "loopback:x".into(),
        };
        hub.offer(
            "b1",
            sid("s1"),
            &NodeId::new("n1").unwrap(),
            media,
            "offer".into(),
        )
        .unwrap();
        assert!(
            matches!(nrx.try_recv(), Ok(ServerMessage::SessionOffer { from, .. }) if from == "b1")
        );
        assert!(
            hub.answer("b1", sid("s1"), "x".into()).is_err(),
            "offerer cannot answer"
        );
        hub.answer("n1c", sid("s1"), "answer".into()).unwrap();
        assert!(
            matches!(brx.try_recv(), Ok(ServerMessage::SessionAnswer { sdp, .. }) if sdp == "answer")
        );
        hub.end("b1", sid("s1"), "done".into()).unwrap();
        assert!(matches!(
            nrx.try_recv(),
            Ok(ServerMessage::SessionEnd { .. })
        ));
        assert!(hub.end("b1", sid("s1"), "again".into()).is_err());
    }

    #[test]
    fn other_users_cannot_reach_a_node() {
        let hub = Hub::default();
        let (tx, _rx) = mpsc::channel(8);
        let (ntx, _nrx) = mpsc::channel(8);
        hub.register("mallory", 2, None, true, tx);
        hub.register("n1c", 1, Some(meta("n1")), true, ntx);
        let e = hub
            .offer(
                "mallory",
                sid("s"),
                &NodeId::new("n1").unwrap(),
                SessionMedia::Speak {
                    destination_id: "d".into(),
                },
                "o".into(),
            )
            .unwrap_err();
        assert_eq!(e.code, "node_offline");
    }

    #[test]
    fn disconnect_ends_sessions_and_reports_offline() {
        let hub = Hub::default();
        let (btx, mut brx) = mpsc::channel(8);
        let (ntx, _nrx) = mpsc::channel(8);
        hub.register("b1", 1, None, true, btx);
        hub.register("n1c", 1, Some(meta("n1")), true, ntx);
        let _ = brx.try_recv();
        hub.offer(
            "b1",
            sid("s1"),
            &NodeId::new("n1").unwrap(),
            SessionMedia::Listen {
                source_id: "a".into(),
            },
            "o".into(),
        )
        .unwrap();
        hub.unregister("n1c");
        assert!(matches!(
            brx.try_recv(),
            Ok(ServerMessage::SessionEnd { .. })
        ));
        assert!(matches!(brx.try_recv(), Ok(ServerMessage::NodeUpdate { node }) if !node.online));
    }

    #[test]
    fn a_device_that_is_not_visible_is_a_visitor() {
        let hub = Hub::default();
        let (btx, mut brx) = mpsc::channel(32);
        let (atx, mut arx) = mpsc::channel(32);
        let (ltx, mut lrx) = mpsc::channel(32);
        hub.register("b1", 1, None, true, btx);
        hub.register("ac", 1, Some(meta("a")), true, atx);
        hub.register("lc", 1, Some(meta("laptop")), true, ltx);
        let laptop = NodeId::new("laptop").unwrap();
        let a = NodeId::new("a").unwrap();
        let listen = || SessionMedia::Listen {
            source_id: "mic".into(),
        };
        let speak = || SessionMedia::Speak {
            destination_id: "out".into(),
        };
        // Visible: others reach the laptop (s1, s3); it reaches "a" (s2, s4).
        hub.offer("b1", sid("s1"), &laptop, listen(), "o".into())
            .unwrap();
        hub.offer("lc", sid("s2"), &a, speak(), "o".into()).unwrap();
        hub.offer("ac", sid("s3"), &laptop, speak(), "o".into())
            .unwrap();
        hub.offer("lc", sid("s4"), &a, listen(), "o".into())
            .unwrap();
        for rx in [&mut brx, &mut arx, &mut lrx] {
            while rx.try_recv().is_ok() {}
        }

        hub.set_visible("lc", false).unwrap();
        // The sessions others have with it end on both sides; the ones it
        // started go on, as a visitor's do.
        let ends = |rx: &mut mpsc::Receiver<ServerMessage>| {
            let mut ended = vec![];
            while let Ok(m) = rx.try_recv() {
                if let ServerMessage::SessionEnd { session_id, reason } = m {
                    assert!(reason.contains("is no longer visible"), "{reason}");
                    ended.push(session_id.as_str().to_owned());
                }
            }
            ended.sort();
            ended
        };
        let gone = |rx: &mut mpsc::Receiver<ServerMessage>| {
            std::iter::from_fn(|| rx.try_recv().ok()).any(|m| {
                matches!(m, ServerMessage::NodeUpdate { node } if node.node_id == laptop && !node.online && !node.sharing)
            })
        };
        assert_eq!(ends(&mut lrx), ["s1", "s3"]);
        assert_eq!(ends(&mut arx), ["s3"]);
        // (brx: the end of s1, then the laptop disappearing.)
        let mut b_msgs: Vec<ServerMessage> = std::iter::from_fn(|| brx.try_recv().ok()).collect();
        assert!(b_msgs.iter().any(|m| matches!(m, ServerMessage::SessionEnd { session_id, .. } if session_id.as_str() == "s1")));
        assert!(b_msgs.drain(..).any(|m| matches!(m, ServerMessage::NodeUpdate { node } if node.node_id == laptop && !node.online)));
        assert!(
            hub.end("lc", sid("s2"), "done".into()).is_ok(),
            "its own session goes on"
        );
        assert!(
            hub.end("lc", sid("s4"), "done".into()).is_ok(),
            "its own session goes on"
        );

        // Listed nowhere, and nobody reaches it; the answer reveals nothing.
        assert_eq!(
            hub.visible_summaries(&[meta("a"), meta("laptop")])
                .iter()
                .map(|n| n.node_id.as_str().to_owned())
                .collect::<Vec<_>>(),
            ["a"]
        );
        for (from, media) in [("b1", listen()), ("ac", speak()), ("ac", listen())] {
            let e = hub
                .offer(from, sid("s5"), &laptop, media, "o".into())
                .unwrap_err();
            assert_eq!(e.code, "node_offline");
            assert_eq!(e.message, "That device is not visible.");
        }
        // It still listens to and sends to the visible devices.
        hub.offer("lc", sid("s6"), &a, listen(), "o".into())
            .unwrap();
        hub.offer("lc", sid("s7"), &a, speak(), "o".into()).unwrap();

        // Visible again: listed and reached again.
        while brx.try_recv().is_ok() {}
        hub.set_visible("lc", true).unwrap();
        assert!(std::iter::from_fn(|| brx.try_recv().ok()).any(|m| {
            matches!(m, ServerMessage::NodeUpdate { node } if node.node_id == laptop && node.online && node.sharing)
        }));
        hub.offer("b1", sid("s8"), &laptop, listen(), "o".into())
            .unwrap();
        // Disconnecting: no longer visible.
        hub.unregister("lc");
        assert!(gone(&mut brx));
        assert!(hub.visible_summaries(&[meta("laptop")]).is_empty());
    }

    #[test]
    fn a_device_that_connects_hidden_is_never_announced() {
        let hub = Hub::default();
        let mut presence = hub.watch_presence().unwrap();
        let (btx, mut brx) = mpsc::channel(16);
        let (htx, _hrx) = mpsc::channel(16);
        hub.register("b1", 1, None, true, btx);
        hub.register("hc", 1, Some(meta("hidden")), false, htx);
        hub.set_endpoints("hc", vec![], vec![]).unwrap();
        hub.node_renamed(1, meta("hidden"));
        assert!(brx.try_recv().is_err(), "nothing said about it");
        assert!(hub.visible_summaries(&[meta("hidden")]).is_empty());
        assert!(presence.try_recv().is_err(), "no notification");
        // Turned visible: announced, and a notification.
        hub.set_visible("hc", true).unwrap();
        assert!(matches!(brx.try_recv(), Ok(ServerMessage::NodeUpdate { node }) if node.online));
        assert!(
            matches!(presence.try_recv(), Ok(Presence::Visible { node_id, .. }) if node_id == "hidden")
        );
        // Hidden again, then gone: one "hidden", nothing for the disconnect.
        hub.set_visible("hc", false).unwrap();
        assert!(
            matches!(presence.try_recv(), Ok(Presence::Hidden { node_id, .. }) if node_id == "hidden")
        );
        hub.unregister("hc");
        assert!(presence.try_recv().is_err());
        // Browsers have no visibility.
        assert_eq!(hub.set_visible("b1", false).unwrap_err().code, "not_a_node");
    }

    #[test]
    fn a_stuck_client_is_dropped_not_waited_for() {
        let hub = Hub::default();
        let (btx, _brx) = mpsc::channel(1); // never drained
        let (ntx, _nrx) = mpsc::channel(8);
        hub.register("slow", 1, None, true, btx);
        hub.register("n1c", 1, Some(meta("n1")), true, ntx); // fills slow's queue
        hub.set_endpoints("n1c", vec![], vec![]).unwrap(); // overflows it
        assert_eq!(
            hub.connection_count(),
            1,
            "slow browser removed; node unaffected"
        );
    }
}
