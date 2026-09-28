//! The result of enumerating endpoints.

use audionet_protocol::{Direction, EndpointDescriptor};
use serde::{Deserialize, Serialize};

/// A problem with one endpoint that did not stop enumeration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumerationWarning {
    /// The endpoint's native identifier, if it could be read.
    pub native_id: Option<String>,
    pub message: String,
}

/// All endpoints found by a backend, in presentation order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndpointInventory {
    endpoints: Vec<EndpointDescriptor>,
    pub warnings: Vec<EnumerationWarning>,
}

impl EndpointInventory {
    /// Builds an inventory, sorting endpoints into a stable, predictable
    /// order: outputs before inputs; within each direction, default
    /// endpoints first, then active before inactive, then by name, then by
    /// ID. OS enumeration order is not guaranteed to be stable, and a
    /// screen-reader user navigating the list benefits from a fixed order.
    pub fn new(mut endpoints: Vec<EndpointDescriptor>, warnings: Vec<EnumerationWarning>) -> Self {
        endpoints.sort_by(|a, b| {
            a.direction
                .cmp(&b.direction)
                .then_with(|| b.is_default().cmp(&a.is_default()))
                .then_with(|| a.state.cmp(&b.state))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        });
        Self {
            endpoints,
            warnings,
        }
    }

    pub fn endpoints(&self) -> &[EndpointDescriptor] {
        &self.endpoints
    }

    pub fn count(&self, direction: Direction) -> usize {
        self.endpoints
            .iter()
            .filter(|e| e.direction == direction)
            .count()
    }

    /// Endpoints of one direction, each with its 1-based position within
    /// that direction (for "Output device 2 of 4").
    pub fn numbered(
        &self,
        direction: Direction,
    ) -> impl Iterator<Item = (usize, &EndpointDescriptor)> {
        self.endpoints
            .iter()
            .filter(move |e| e.direction == direction)
            .enumerate()
            .map(|(i, e)| (i + 1, e))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use audionet_protocol::{
        AudioBackend, DefaultRole, EndpointId, EndpointState, LoopbackSupport,
    };

    pub(crate) fn endpoint(
        id: &str,
        name: &str,
        direction: Direction,
        state: EndpointState,
        default: bool,
    ) -> EndpointDescriptor {
        EndpointDescriptor {
            id: EndpointId::new(AudioBackend::Wasapi, id).unwrap(),
            direction,
            name: name.into(),
            description: None,
            adapter: None,
            state,
            default_roles: if default {
                vec![DefaultRole::Console]
            } else {
                vec![]
            },
            format: None,
            loopback: LoopbackSupport::NotApplicable,
        }
    }

    #[test]
    fn orders_outputs_first_then_default_active_name() {
        use Direction::*;
        use EndpointState::*;
        let inv = EndpointInventory::new(
            vec![
                endpoint("1", "Microphone", Input, Active, false),
                endpoint("2", "b speakers", Output, Active, false),
                endpoint("3", "Headset", Input, Active, true),
                endpoint("4", "A speakers", Output, Unplugged, false),
                endpoint("5", "Z headphones", Output, Active, true),
                endpoint("6", "a speakers", Output, Active, false),
            ],
            vec![],
        );
        let ids: Vec<_> = inv.endpoints().iter().map(|e| e.id.native_id()).collect();
        assert_eq!(ids, ["5", "6", "2", "4", "3", "1"]);
    }

    #[test]
    fn numbers_per_direction() {
        use Direction::*;
        use EndpointState::*;
        let inv = EndpointInventory::new(
            vec![
                endpoint("1", "Mic", Input, Active, false),
                endpoint("2", "Out A", Output, Active, false),
                endpoint("3", "Out B", Output, Active, false),
            ],
            vec![],
        );
        assert_eq!(inv.count(Output), 2);
        assert_eq!(inv.count(Input), 1);
        let outs: Vec<_> = inv
            .numbered(Output)
            .map(|(n, e)| (n, e.name.as_str()))
            .collect();
        assert_eq!(outs, [(1, "Out A"), (2, "Out B")]);
        let ins: Vec<_> = inv.numbered(Input).map(|(n, _)| n).collect();
        assert_eq!(ins, [1]);
    }
}
