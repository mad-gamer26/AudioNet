//! Choosing an endpoint on the command line.
//!
//! A selector is `default`, a position number as printed by `audionet list`
//! ("Input device 3 of 11" → `3`), or an exact endpoint identifier. Position
//! numbers exist so a screen-reader user can pick a device without copying
//! a long identifier.

use core::fmt;
use core::str::FromStr;

use audionet_audio::EndpointInventory;
use audionet_protocol::{DefaultRole, Direction, EndpointDescriptor};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointSelector {
    Default,
    Position(usize),
    Id(String),
}

impl FromStr for EndpointSelector {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("the device selector is empty".into());
        }
        if s.eq_ignore_ascii_case("default") {
            return Ok(EndpointSelector::Default);
        }
        if s.bytes().all(|b| b.is_ascii_digit()) {
            return match s.parse::<usize>() {
                Ok(0) | Err(_) => Err(format!("{s} is not a valid device number")),
                Ok(n) => Ok(EndpointSelector::Position(n)),
            };
        }
        Ok(EndpointSelector::Id(s.to_owned()))
    }
}

/// Why a selector matched nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectError(pub String);

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SelectError {}

/// A selected endpoint with its position for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected<'a> {
    pub endpoint: &'a EndpointDescriptor,
    pub position: usize,
    pub total: usize,
}

pub fn select<'a>(
    inventory: &'a EndpointInventory,
    direction: Direction,
    selector: &EndpointSelector,
) -> Result<Selected<'a>, SelectError> {
    let noun = direction.label().to_lowercase();
    let total = inventory.count(direction);
    let found = match selector {
        EndpointSelector::Default => inventory
            .numbered(direction)
            .find(|(_, e)| e.default_roles.contains(&DefaultRole::Console))
            .ok_or_else(|| SelectError(format!("There is no default {noun} device."))),
        EndpointSelector::Position(n) => {
            inventory.numbered(direction).nth(n - 1).ok_or_else(|| {
                SelectError(match total {
                    0 => format!("There are no {noun} devices."),
                    1 => format!("There is no {noun} device {n}; there is only 1."),
                    _ => format!("There is no {noun} device {n}; there are {total}."),
                })
            })
        }
        EndpointSelector::Id(id) => inventory
            .numbered(direction)
            .find(|(_, e)| e.id.native_id() == id)
            .ok_or_else(|| {
                let other_direction = inventory.endpoints().iter().any(|e| e.id.native_id() == id);
                SelectError(if other_direction {
                    format!("The device {id} is not an {noun} device.")
                } else {
                    format!("No {noun} device has the identifier {id}.")
                })
            }),
    }?;
    Ok(Selected {
        endpoint: found.1,
        position: found.0,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use audionet_protocol::{AudioBackend, EndpointId, EndpointState, LoopbackSupport};

    fn inventory() -> EndpointInventory {
        let e = |id: &str, direction, default: bool| EndpointDescriptor {
            id: EndpointId::new(AudioBackend::Wasapi, id).unwrap(),
            direction,
            name: id.to_uppercase(),
            description: None,
            adapter: None,
            state: EndpointState::Active,
            default_roles: if default {
                vec![DefaultRole::Console]
            } else {
                vec![]
            },
            format: None,
            loopback: LoopbackSupport::Expected,
        };
        EndpointInventory::new(
            vec![
                e("out-a", Direction::Output, false),
                e("out-b", Direction::Output, true),
                e("mic", Direction::Input, false),
            ],
            vec![],
        )
    }

    fn sel(s: &str) -> EndpointSelector {
        s.parse().unwrap()
    }

    #[test]
    fn parses_selectors() {
        assert_eq!(sel("DEFAULT"), EndpointSelector::Default);
        assert_eq!(sel(" 3 "), EndpointSelector::Position(3));
        assert_eq!(
            sel("{0.0.1}.{x}"),
            EndpointSelector::Id("{0.0.1}.{x}".into())
        );
        assert!("0".parse::<EndpointSelector>().is_err());
        assert!("".parse::<EndpointSelector>().is_err());
    }

    #[test]
    fn selects_by_default_position_and_id() {
        let inv = inventory();
        let d = select(&inv, Direction::Output, &sel("default")).unwrap();
        assert_eq!(
            (d.endpoint.id.native_id(), d.position, d.total),
            ("out-b", 1, 2)
        );
        let p = select(&inv, Direction::Output, &sel("2")).unwrap();
        assert_eq!(p.endpoint.id.native_id(), "out-a");
        let i = select(&inv, Direction::Input, &sel("mic")).unwrap();
        assert_eq!((i.position, i.total), (1, 1));
    }

    #[test]
    fn explains_failures_in_words() {
        let inv = inventory();
        let err = |d, s| select(&inv, d, &sel(s)).unwrap_err().0;
        assert_eq!(
            err(Direction::Output, "5"),
            "There is no output device 5; there are 2."
        );
        assert_eq!(
            err(Direction::Input, "2"),
            "There is no input device 2; there is only 1."
        );
        assert_eq!(
            err(Direction::Input, "default"),
            "There is no default input device."
        );
        assert_eq!(
            err(Direction::Input, "out-a"),
            "The device out-a is not an input device."
        );
        assert_eq!(
            err(Direction::Output, "nope"),
            "No output device has the identifier nope."
        );
    }
}
