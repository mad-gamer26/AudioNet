//! AudioNet nodes.

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;
use crate::version::ProtocolVersion;

/// The kind of system a node runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    Ios,
    Android,
    Browser,
}

/// A participating AudioNet instance: a computer, phone, browser tab, or
/// server. One physical machine may run more than one node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDescriptor {
    pub id: NodeId,
    /// A user-facing name, e.g. "Studio PC".
    pub display_name: String,
    pub platform: Platform,
    pub protocol_version: ProtocolVersion,
}
