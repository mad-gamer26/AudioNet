//! Resolving saved endpoint references against the current inventory.
//!
//! Endpoint IDs can be persisted, but they are not permanent: driver updates,
//! device reinstallation, hardware changes and virtual-cable updates can
//! remove an endpoint or recreate it under a new ID. Saved configuration must
//! therefore be resolved each time and must represent a missing endpoint
//! explicitly instead of failing silently or guessing.

use audionet_protocol::{Direction, EndpointDescriptor, EndpointId, EndpointState};
use serde::{Deserialize, Serialize};

/// What a saved configuration remembers about an endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedEndpointRef {
    pub id: EndpointId,
    pub direction: Direction,
    /// The display name when the reference was saved, used only to suggest a
    /// replacement if the ID has disappeared.
    pub name: String,
}

/// The outcome of resolving a [`SavedEndpointRef`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointResolution<'a> {
    /// The saved ID exists and is active.
    Available(&'a EndpointDescriptor),
    /// The saved ID exists but is disabled, unplugged or not present.
    Unavailable {
        endpoint: &'a EndpointDescriptor,
        state: EndpointState,
    },
    /// The saved ID is gone, but exactly one active endpoint with the same
    /// direction and name exists. This is only a *suggestion*: it must not
    /// be used without the user confirming it, because two different devices
    /// can share a name.
    MissingWithCandidate(&'a EndpointDescriptor),
    /// The saved ID is gone and there is no unambiguous replacement.
    Missing,
}

/// Resolves a saved endpoint reference against the current endpoints.
pub fn resolve_saved_endpoint<'a>(
    saved: &SavedEndpointRef,
    current: &'a [EndpointDescriptor],
) -> EndpointResolution<'a> {
    if let Some(found) = current
        .iter()
        .find(|e| e.id == saved.id && e.direction == saved.direction)
    {
        return if found.state == EndpointState::Active {
            EndpointResolution::Available(found)
        } else {
            EndpointResolution::Unavailable {
                endpoint: found,
                state: found.state,
            }
        };
    }
    let mut candidates = current.iter().filter(|e| {
        e.state == EndpointState::Active
            && e.direction == saved.direction
            && e.id.backend() == saved.id.backend()
            && e.name == saved.name
    });
    match (candidates.next(), candidates.next()) {
        (Some(only), None) => EndpointResolution::MissingWithCandidate(only),
        _ => EndpointResolution::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::tests::endpoint;
    use Direction::*;
    use EndpointState::*;
    use audionet_protocol::AudioBackend;

    fn saved(id: &str, name: &str) -> SavedEndpointRef {
        SavedEndpointRef {
            id: EndpointId::new(AudioBackend::Wasapi, id).unwrap(),
            direction: Output,
            name: name.into(),
        }
    }

    #[test]
    fn exact_id_active() {
        let current = [endpoint("a", "Speakers", Output, Active, false)];
        assert_eq!(
            resolve_saved_endpoint(&saved("a", "Speakers"), &current),
            EndpointResolution::Available(&current[0])
        );
    }

    #[test]
    fn exact_id_inactive_is_reported_with_state() {
        let current = [endpoint("a", "Speakers", Output, Unplugged, false)];
        assert_eq!(
            resolve_saved_endpoint(&saved("a", "Speakers"), &current),
            EndpointResolution::Unavailable {
                endpoint: &current[0],
                state: Unplugged
            }
        );
    }

    #[test]
    fn recreated_endpoint_is_only_a_candidate() {
        let current = [endpoint("new-id", "Speakers", Output, Active, false)];
        assert_eq!(
            resolve_saved_endpoint(&saved("old-id", "Speakers"), &current),
            EndpointResolution::MissingWithCandidate(&current[0])
        );
    }

    #[test]
    fn ambiguous_or_wrong_direction_is_missing() {
        let current = [
            endpoint("x", "Speakers", Output, Active, false),
            endpoint("y", "Speakers", Output, Active, false),
            endpoint("z", "Line", Input, Active, false),
        ];
        assert_eq!(
            resolve_saved_endpoint(&saved("old", "Speakers"), &current),
            EndpointResolution::Missing
        );
        // Same ID but the saved direction differs: not a match.
        assert_eq!(
            resolve_saved_endpoint(&saved("z", "Line"), &current),
            EndpointResolution::Missing
        );
    }

    #[test]
    fn inactive_name_match_is_not_a_candidate() {
        let current = [endpoint("new", "Speakers", Output, Disabled, false)];
        assert_eq!(
            resolve_saved_endpoint(&saved("old", "Speakers"), &current),
            EndpointResolution::Missing
        );
    }
}
