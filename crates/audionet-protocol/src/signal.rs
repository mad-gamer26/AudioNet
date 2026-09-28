//! Signaling messages exchanged with an AudioNet coordination server over
//! a WebSocket (`/api/v1/ws`), as JSON text frames.
//!
//! The server relays session negotiation between an *offerer* (a browser or
//! another node) and a *node* that owns audio sources and destinations. It
//! never touches media: audio flows peer to peer over WebRTC (DTLS-SRTP),
//! relayed through TURN only when a direct path is impossible.
//!
//! Negotiation is non-trickle: the offerer gathers all ICE candidates
//! (including TURN relay candidates) before sending its offer, and the
//! node's answer contains all of its candidates. One offer, one answer, one
//! end message per session.
//!
//! Nothing here names a particular server. The server URL is configuration.
//! See `docs/protocol.md` §10 for the message reference.

use serde::{Deserialize, Serialize};

use crate::ids::{NodeId, SessionId};
use crate::node::Platform;
use crate::version::ProtocolVersion;

/// The kind of client on a signaling connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// A device that owns audio endpoints (Windows, macOS, phone app...).
    Node,
    /// A browser tab using the web client.
    Browser,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub kind: ClientKind,
    /// Software name and version, e.g. "audionet-node 0.1.0".
    pub software: String,
    pub platform: Option<Platform>,
}

/// What a source captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// A microphone or other input device.
    Input,
    /// What an output device is playing (system audio).
    Loopback,
}

/// A source a node offers. `id` is opaque and stable while the node runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: String,
    pub name: String,
    pub source_type: SourceType,
    pub is_default: bool,
}

/// A destination (output device) a node offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// A node as the server presents it to the node's owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSummary {
    pub node_id: NodeId,
    pub name: String,
    pub platform: Option<Platform>,
    /// The device's AudioNet is running and connected.
    pub online: bool,
    /// The device shares its audio: others may listen to its sources and
    /// it may send its own. An online device that does not share still
    /// receives audio (others may send to its outputs, and it may listen to
    /// them). Always false when offline. Servers that do not send it (before
    /// sharing existed) meant "sharing whenever online".
    #[serde(default = "sharing_when_unsaid")]
    pub sharing: bool,
    pub sources: Vec<SourceInfo>,
    pub destinations: Vec<DestinationInfo>,
}

fn sharing_when_unsaid() -> bool {
    true
}

/// What a session carries, from the offerer's point of view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionMedia {
    /// The node sends audio from `source_id` to the offerer.
    Listen { source_id: String },
    /// The offerer sends audio to the node, which plays it on `destination_id`.
    Speak { destination_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Starting,
    Active,
    Ended,
    Failed,
}

/// An ICE server for `RTCPeerConnection` (TURN credentials are short-lived).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

/// Messages a client sends to the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message on every connection.
    Hello {
        protocol_version: ProtocolVersion,
        client: ClientInfo,
    },
    /// Nodes: whether this device shares its audio (see
    /// [`NodeSummary::sharing`]). Until a node says, the server takes it as
    /// sharing, as nodes that connected only to share did.
    Sharing {
        sharing: bool,
    },
    /// Nodes: the sources and destinations currently available.
    Endpoints {
        sources: Vec<SourceInfo>,
        destinations: Vec<DestinationInfo>,
    },
    /// Browsers: request the list of the user's nodes.
    ListNodes,
    /// Offerer → node: start a session.
    SessionOffer {
        session_id: SessionId,
        node_id: NodeId,
        media: SessionMedia,
        sdp: String,
    },
    /// Node → offerer.
    SessionAnswer {
        session_id: SessionId,
        sdp: String,
    },
    /// Either side: progress or failure, in words.
    SessionStatus {
        session_id: SessionId,
        state: SessionState,
        detail: Option<String>,
    },
    /// Either side: stop a session.
    SessionEnd {
        session_id: SessionId,
        reason: String,
    },
    Ping {
        nonce: u64,
    },
}

/// Messages the server sends to a client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        protocol_version: ProtocolVersion,
        /// Server-assigned id for this connection.
        connection_id: String,
        username: String,
        /// Set when this connection is an authenticated node.
        node_id: Option<NodeId>,
        ice_servers: Vec<IceServer>,
    },
    Nodes {
        nodes: Vec<NodeSummary>,
    },
    NodeUpdate {
        node: NodeSummary,
    },
    SessionOffer {
        session_id: SessionId,
        /// Connection id of the offerer.
        from: String,
        media: SessionMedia,
        sdp: String,
    },
    SessionAnswer {
        session_id: SessionId,
        sdp: String,
    },
    SessionStatus {
        session_id: SessionId,
        state: SessionState,
        detail: Option<String>,
    },
    SessionEnd {
        session_id: SessionId,
        reason: String,
    },
    /// A request failed; `message` is suitable for showing to the user.
    Error {
        code: String,
        message: String,
        /// The session the problem ends, when it concerns one (an offer the
        /// server could not deliver: the device is offline, or does not
        /// share). Older servers leave it out.
        #[serde(default)]
        session_id: Option<SessionId>,
    },
    Pong {
        nonce: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes() {
        let m = ClientMessage::SessionOffer {
            session_id: SessionId::new("s1").unwrap(),
            node_id: NodeId::new("n1").unwrap(),
            media: SessionMedia::Listen {
                source_id: "loopback:abc".into(),
            },
            sdp: "v=0".into(),
        };
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::json!({
                "type": "session_offer",
                "session_id": "s1",
                "node_id": "n1",
                "media": { "kind": "listen", "source_id": "loopback:abc" },
                "sdp": "v=0"
            })
        );
        let back: ClientMessage =
            serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        let ping: ClientMessage = serde_json::from_str(r#"{"type":"ping","nonce":5}"#).unwrap();
        assert_eq!(ping, ClientMessage::Ping { nonce: 5 });
        let e = ServerMessage::Error {
            code: "not_found".into(),
            message: "That device is offline.".into(),
            session_id: None,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"error","code":"not_found","message":"That device is offline.","session_id":null}"#
        );
        // From a server before session_id.
        let old: ServerMessage =
            serde_json::from_str(r#"{"type":"error","code":"x","message":"y"}"#).unwrap();
        assert!(matches!(
            old,
            ServerMessage::Error {
                session_id: None,
                ..
            }
        ));
    }

    #[test]
    fn sharing_on_the_wire() {
        let m = ClientMessage::Sharing { sharing: false };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"type":"sharing","sharing":false}"#
        );
        // A device list from a server that predates sharing.
        let old = r#"{"node_id":"n1","name":"PC","platform":null,"online":true,"sources":[],"destinations":[]}"#;
        let n: NodeSummary = serde_json::from_str(old).unwrap();
        assert!(n.sharing, "online meant sharing before sharing existed");
    }

    #[test]
    fn rejects_unknown_types() {
        assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"bogus"}"#).is_err());
    }
}
