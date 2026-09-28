//! The signaling hub: who is connected, which nodes are online with which
//! endpoints, and which sessions route between which connections.
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
    /// Nodes: whether the device shares its audio (until it says, yes: as
    /// devices that connected only to share did).
    sharing: bool,
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
    /// Which side sends the audio: the node (listen) or the offerer (speak).
    node_sends: bool,
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

#[derive(Debug, Default)]
pub struct Hub {
    inner: Mutex<Inner>,
}

impl Hub {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers a connection. For a node, replaces any older connection of
    /// the same node (whose sessions end).
    pub fn register(
        &self,
        conn_id: &str,
        user_id: i64,
        node: Option<NodeMeta>,
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
                sharing: true,
                tx,
            },
        );
        if let Some(meta) = g.conns.get(conn_id).and_then(|c| c.node.clone()) {
            Self::broadcast_node(&mut g, user_id, &meta);
        }
    }

    /// Removes a connection, ends its sessions, and reports nodes offline.
    pub fn unregister(&self, conn_id: &str) {
        let mut g = self.lock();
        let Some(conn) = g.conns.remove(conn_id) else {
            return;
        };
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
        if let Some(meta) = conn.node {
            if g.node_conn.get(&meta.id).is_some_and(|c| c == conn_id) {
                g.node_conn.remove(&meta.id);
                g.endpoints.remove(&meta.id);
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

    fn summary(g: &Inner, meta: &NodeMeta) -> NodeSummary {
        let conn = g.node_conn.get(&meta.id).and_then(|c| g.conns.get(c));
        let online = conn.is_some();
        let sharing = conn.is_some_and(|c| c.sharing);
        let (sources, destinations) = g.endpoints.get(&meta.id).cloned().unwrap_or_default();
        NodeSummary {
            node_id: NodeId::new(meta.id.clone())
                .unwrap_or_else(|_| NodeId::new("invalid").expect("valid")),
            name: meta.name.clone(),
            platform: meta.platform,
            online,
            sharing,
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
        Self::broadcast_node(&mut g, user_id, &meta);
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
        g.endpoints.insert(meta.id.clone(), (sources, destinations));
        Self::broadcast_node(&mut g, user_id, &meta);
        Ok(())
    }

    /// A node starts or stops sharing its audio. Stopping ends every session
    /// in which it sends (others listening to it, and what it sends to
    /// others), telling both sides; what it receives goes on.
    pub fn set_sharing(&self, conn_id: &str, sharing: bool) -> Result<(), HubError> {
        let mut g = self.lock();
        let conn = g
            .conns
            .get_mut(conn_id)
            .ok_or_else(|| herr("gone", "Connection closed."))?;
        let meta = conn
            .node
            .clone()
            .ok_or_else(|| herr("not_a_node", "Only devices share audio."))?;
        let user_id = conn.user_id;
        if conn.sharing == sharing {
            return Ok(());
        }
        conn.sharing = sharing;
        if !sharing {
            let ended: Vec<(String, String, String)> = g
                .sessions
                .iter()
                .filter(|(_, r)| {
                    (r.node_conn == conn_id && r.node_sends)
                        || (r.offerer == conn_id && !r.node_sends)
                })
                .map(|(id, r)| (id.clone(), r.offerer.clone(), r.node_conn.clone()))
                .collect();
            let reason = format!("\"{}\" stopped sharing its audio.", meta.name);
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
        Ok(())
    }

    /// Summaries for a user's nodes, given their database rows.
    pub fn summaries(&self, nodes: &[NodeMeta]) -> Vec<NodeSummary> {
        let g = self.lock();
        nodes.iter().map(|m| Self::summary(&g, m)).collect()
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
        let node_conn = g
            .node_conn
            .get(node_id.as_str())
            .cloned()
            .ok_or_else(|| herr("node_offline", "That device is not connected."))?;
        if g.conns.get(&node_conn).map(|c| c.user_id) != Some(user_id) {
            // Same message as offline: do not reveal other users' devices.
            return Err(herr("node_offline", "That device is not connected."));
        }
        if node_conn == from {
            return Err(herr(
                "invalid",
                "A device cannot start a session with itself.",
            ));
        }
        // Only a device that shares sends audio: the device listened to, or
        // a device sending to another. (Browsers always may send.)
        let node_sends = matches!(media, SessionMedia::Listen { .. });
        let sender = if node_sends { &node_conn } else { from };
        if let Some(c) = g.conns.get(sender).filter(|c| !c.sharing) {
            let name = c.node.as_ref().map_or("That device", |n| n.name.as_str());
            return Err(herr(
                "not_sharing",
                if sender == from {
                    "This device is not sharing its audio. Start sharing to send it.".to_owned()
                } else {
                    format!("\"{name}\" is not sharing its audio.")
                },
            ));
        }
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
                node_sends,
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
        hub.register("b1", 1, None, btx);
        hub.register("n1c", 1, Some(meta("n1")), ntx);
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
        hub.register("mallory", 2, None, tx);
        hub.register("n1c", 1, Some(meta("n1")), ntx);
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
        hub.register("b1", 1, None, btx);
        hub.register("n1c", 1, Some(meta("n1")), ntx);
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
    fn not_sharing_devices_receive_but_do_not_send() {
        let hub = Hub::default();
        let (btx, mut brx) = mpsc::channel(16);
        let (atx, mut arx) = mpsc::channel(16);
        let (ltx, mut lrx) = mpsc::channel(16);
        hub.register("b1", 1, None, btx);
        hub.register("ac", 1, Some(meta("a")), atx);
        hub.register("lc", 1, Some(meta("laptop")), ltx);
        let laptop = NodeId::new("laptop").unwrap();
        let listen = || SessionMedia::Listen {
            source_id: "mic".into(),
        };
        let speak = || SessionMedia::Speak {
            destination_id: "out".into(),
        };
        // Sharing: listened to, and sending to "a".
        hub.offer("b1", sid("s1"), &laptop, listen(), "o".into())
            .unwrap();
        hub.offer(
            "lc",
            sid("s2"),
            &NodeId::new("a").unwrap(),
            speak(),
            "o".into(),
        )
        .unwrap();
        // Receiving: "a" sends to the laptop.
        hub.offer("ac", sid("s3"), &laptop, speak(), "o".into())
            .unwrap();
        while brx.try_recv().is_ok() {}
        while arx.try_recv().is_ok() {}
        while lrx.try_recv().is_ok() {}

        hub.set_sharing("lc", false).unwrap();
        // Both sending sessions end on both sides; the receiving one goes on.
        let ends = |rx: &mut mpsc::Receiver<ServerMessage>| {
            let mut ended = vec![];
            while let Ok(m) = rx.try_recv() {
                if let ServerMessage::SessionEnd { session_id, reason } = m {
                    assert!(reason.contains("stopped sharing"), "{reason}");
                    ended.push(session_id.as_str().to_owned());
                }
            }
            ended.sort();
            ended
        };
        assert_eq!(ends(&mut lrx), ["s1", "s2"]);
        assert_eq!(ends(&mut brx), ["s1"]);
        assert_eq!(ends(&mut arx), ["s2"]);
        assert!(
            hub.end("ac", sid("s3"), "done".into()).is_ok(),
            "receiving goes on"
        );

        // Others see it online but not sharing.
        let s = hub.summaries(&[meta("laptop")]);
        assert!(s[0].online && !s[0].sharing);
        // Nobody may listen to it; it may not send; it may still receive.
        let e = hub
            .offer("b1", sid("s4"), &laptop, listen(), "o".into())
            .unwrap_err();
        assert_eq!(e.code, "not_sharing");
        assert!(e.message.contains("is not sharing"), "{}", e.message);
        let e = hub
            .offer(
                "lc",
                sid("s5"),
                &NodeId::new("a").unwrap(),
                speak(),
                "o".into(),
            )
            .unwrap_err();
        assert_eq!(e.code, "not_sharing");
        hub.offer("ac", sid("s6"), &laptop, speak(), "o".into())
            .unwrap();
        hub.offer(
            "lc",
            sid("s7"),
            &NodeId::new("a").unwrap(),
            listen(),
            "o".into(),
        )
        .unwrap();
        // Sharing again: listened to again.
        hub.set_sharing("lc", true).unwrap();
        hub.offer("b1", sid("s8"), &laptop, listen(), "o".into())
            .unwrap();
        // Offline is not sharing.
        hub.unregister("lc");
        let s = hub.summaries(&[meta("laptop")]);
        assert!(!s[0].online && !s[0].sharing);
    }

    #[test]
    fn a_stuck_client_is_dropped_not_waited_for() {
        let hub = Hub::default();
        let (btx, _brx) = mpsc::channel(1); // never drained
        let (ntx, _nrx) = mpsc::channel(8);
        hub.register("slow", 1, None, btx);
        hub.register("n1c", 1, Some(meta("n1")), ntx); // fills slow's queue
        hub.set_endpoints("n1c", vec![], vec![]).unwrap(); // overflows it
        assert_eq!(
            hub.connection_count(),
            1,
            "slow browser removed; node unaffected"
        );
    }
}
