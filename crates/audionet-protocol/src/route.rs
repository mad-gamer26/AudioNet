//! The logical routing model: `AudioSource → AudioRoute → AudioDestination`.
//!
//! A route says *what* audio should go *where*. It says nothing about how the
//! audio travels: a route is not a socket, a WebRTC peer connection, or a
//! stream session. The transport layer derives sessions from routes.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{EndpointId, NodeId, RouteId};

/// Upper bound on destinations per route, so untrusted route descriptions
/// cannot request unbounded fan-out.
pub const MAX_DESTINATIONS_PER_ROUTE: usize = 32;

/// What produces the audio, on the node that owns it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceKind {
    /// Record from an input endpoint (microphone, line in, virtual cable).
    EndpointCapture { endpoint: EndpointId },
    /// Capture what an output endpoint is playing (system audio).
    EndpointLoopback { endpoint: EndpointId },
    /// Capture one application's audio where the platform supports it.
    ProcessCapture {
        /// Executable file name, e.g. "game.exe". Process IDs are not stable
        /// enough to persist, so they are resolved at session start.
        executable: String,
        /// Whether child processes are included.
        include_process_tree: bool,
    },
    /// The node's platform-chosen input, for platforms (browsers, phones)
    /// where the OS or the user picks the microphone.
    NodeDefaultInput,
}

/// An `AudioSource`: a source kind on a specific node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    pub node: NodeId,
    pub kind: SourceKind,
}

/// What consumes the audio, on the node that owns it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DestinationKind {
    /// Play to a specific output endpoint.
    EndpointRender { endpoint: EndpointId },
    /// Play to the node's platform-chosen output.
    NodeDefaultOutput,
}

/// An `AudioDestination`: a destination kind on a specific node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DestinationRef {
    pub node: NodeId,
    pub kind: DestinationKind,
}

/// A logical path from one source to one or more destinations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRoute {
    pub id: RouteId,
    pub source: SourceRef,
    pub destinations: Vec<DestinationRef>,
}

/// Why a route description is invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteError {
    NoDestinations,
    TooManyDestinations {
        count: usize,
        max: usize,
    },
    DuplicateDestination {
        index: usize,
    },
    /// The route would play an endpoint's loopback capture back into the
    /// same endpoint, creating a feedback loop.
    FeedbackLoop {
        index: usize,
    },
    EmptyExecutableName,
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteError::NoDestinations => f.write_str("route has no destinations"),
            RouteError::TooManyDestinations { count, max } => {
                write!(f, "route has {count} destinations; the maximum is {max}")
            }
            RouteError::DuplicateDestination { index } => {
                write!(
                    f,
                    "destination {} duplicates an earlier destination",
                    index + 1
                )
            }
            RouteError::FeedbackLoop { index } => write!(
                f,
                "destination {} plays into the same endpoint the route captures by loopback",
                index + 1
            ),
            RouteError::EmptyExecutableName => {
                f.write_str("process capture source has an empty executable name")
            }
        }
    }
}

impl std::error::Error for RouteError {}

impl AudioRoute {
    /// Checks structural validity. Does not check that nodes or endpoints exist.
    pub fn validate(&self) -> Result<(), RouteError> {
        if self.destinations.is_empty() {
            return Err(RouteError::NoDestinations);
        }
        if self.destinations.len() > MAX_DESTINATIONS_PER_ROUTE {
            return Err(RouteError::TooManyDestinations {
                count: self.destinations.len(),
                max: MAX_DESTINATIONS_PER_ROUTE,
            });
        }
        if let SourceKind::ProcessCapture { executable, .. } = &self.source.kind {
            if executable.trim().is_empty() {
                return Err(RouteError::EmptyExecutableName);
            }
        }
        for (index, dest) in self.destinations.iter().enumerate() {
            if self.destinations[..index].contains(dest) {
                return Err(RouteError::DuplicateDestination { index });
            }
            if let (
                SourceKind::EndpointLoopback { endpoint: src },
                DestinationKind::EndpointRender { endpoint: dst },
            ) = (&self.source.kind, &dest.kind)
            {
                if src == dst && self.source.node == dest.node {
                    return Err(RouteError::FeedbackLoop { index });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AudioBackend;

    fn node(name: &str) -> NodeId {
        NodeId::new(name).unwrap()
    }

    fn endpoint(id: &str) -> EndpointId {
        EndpointId::new(AudioBackend::Wasapi, id).unwrap()
    }

    fn loopback_route(dests: Vec<DestinationRef>) -> AudioRoute {
        AudioRoute {
            id: RouteId::new("r1").unwrap(),
            source: SourceRef {
                node: node("pc"),
                kind: SourceKind::EndpointLoopback {
                    endpoint: endpoint("speakers"),
                },
            },
            destinations: dests,
        }
    }

    fn phone_out() -> DestinationRef {
        DestinationRef {
            node: node("phone"),
            kind: DestinationKind::NodeDefaultOutput,
        }
    }

    #[test]
    fn accepts_one_to_many() {
        let browser = DestinationRef {
            node: node("browser"),
            kind: DestinationKind::NodeDefaultOutput,
        };
        assert_eq!(
            loopback_route(vec![phone_out(), browser]).validate(),
            Ok(())
        );
    }

    #[test]
    fn rejects_structural_errors() {
        assert_eq!(
            loopback_route(vec![]).validate(),
            Err(RouteError::NoDestinations)
        );
        assert_eq!(
            loopback_route(vec![phone_out(), phone_out()]).validate(),
            Err(RouteError::DuplicateDestination { index: 1 })
        );
        let too_many = (0..=MAX_DESTINATIONS_PER_ROUTE)
            .map(|i| DestinationRef {
                node: node(&format!("n{i}")),
                kind: DestinationKind::NodeDefaultOutput,
            })
            .collect();
        assert!(matches!(
            loopback_route(too_many).validate(),
            Err(RouteError::TooManyDestinations { .. })
        ));
    }

    #[test]
    fn rejects_loopback_feedback_on_same_node_only() {
        let same = DestinationRef {
            node: node("pc"),
            kind: DestinationKind::EndpointRender {
                endpoint: endpoint("speakers"),
            },
        };
        assert_eq!(
            loopback_route(vec![phone_out(), same]).validate(),
            Err(RouteError::FeedbackLoop { index: 1 })
        );
        // Same native ID string on a different node is a different endpoint.
        let other_node = DestinationRef {
            node: node("laptop"),
            kind: DestinationKind::EndpointRender {
                endpoint: endpoint("speakers"),
            },
        };
        assert_eq!(loopback_route(vec![other_node]).validate(), Ok(()));
    }

    #[test]
    fn serializes_with_type_tags() {
        let route = loopback_route(vec![phone_out()]);
        let json = serde_json::to_value(&route).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "r1",
                "source": {
                    "node": "pc",
                    "kind": {
                        "type": "endpoint_loopback",
                        "endpoint": { "backend": "wasapi", "native_id": "speakers" }
                    }
                },
                "destinations": [
                    { "node": "phone", "kind": { "type": "node_default_output" } }
                ]
            })
        );
        assert_eq!(serde_json::from_value::<AudioRoute>(json).unwrap(), route);
    }
}
