//! Output for `audionet list`.
//!
//! The text format is designed for screen readers: one fact per line, a
//! "label: value" pattern, the same set of lines for every endpoint, and a
//! position ("Output device 2 of 4") at the start of each entry. It never
//! relies on columns, tables, color or cursor movement.
//!
//! The JSON format is documented in `docs/protocol.md` and versioned by
//! [`ENDPOINT_LIST_SCHEMA_VERSION`].

use core::fmt::Write as _;

use audionet_audio::{EndpointInventory, EnumerationWarning};
use audionet_protocol::{AudioBackend, Direction, EndpointDescriptor};
use serde::Serialize;

pub const ENDPOINT_LIST_SCHEMA: &str = "audionet.endpoint_list";
/// Bump when a field is removed, renamed or changes meaning. Adding a field
/// does not require a bump; consumers must ignore unknown fields.
pub const ENDPOINT_LIST_SCHEMA_VERSION: u32 = 1;

fn count_phrase(n: usize, noun: &str) -> String {
    match n {
        0 => format!("no {noun}s"),
        1 => format!("1 {noun}"),
        _ => format!("{n} {noun}s"),
    }
}

/// Renders the inventory as screen-reader-friendly plain text.
pub fn render_text(inventory: &EndpointInventory) -> String {
    let outputs = inventory.count(Direction::Output);
    let inputs = inventory.count(Direction::Input);
    let mut out = String::new();

    if outputs + inputs == 0 {
        out.push_str("AudioNet found no audio devices.\n");
    } else {
        let _ = writeln!(
            out,
            "AudioNet found {} and {}.",
            count_phrase(outputs, "output device"),
            count_phrase(inputs, "input device")
        );
    }

    for direction in [Direction::Output, Direction::Input] {
        let total = inventory.count(direction);
        for (position, endpoint) in inventory.numbered(direction) {
            out.push('\n');
            write_endpoint(&mut out, endpoint, position, total);
        }
    }

    write_warnings(&mut out, &inventory.warnings);
    out
}

fn write_endpoint(out: &mut String, e: &EndpointDescriptor, position: usize, total: usize) {
    let _ = writeln!(
        out,
        "{} device {position} of {total}: {}",
        e.direction.label(),
        e.name
    );
    let _ = writeln!(out, "Identifier: {}", e.id);
    let _ = writeln!(out, "Status: {}", e.state.label());
    let defaults = if e.default_roles.is_empty() {
        "No".to_owned()
    } else {
        let roles: Vec<_> = e.default_roles.iter().map(|r| r.label()).collect();
        format!("Yes, for {}", roles.join(", "))
    };
    let _ = writeln!(out, "Default device: {defaults}");
    match &e.format {
        Some(f) => {
            let _ = writeln!(out, "Channels: {}", f.channels);
            let _ = writeln!(out, "Sample rate: {} Hz", f.sample_rate_hz);
            let _ = writeln!(out, "Sample format: {}", f.sample_format.describe());
        }
        None => {
            out.push_str("Channels: Unknown\nSample rate: Unknown\nSample format: Unknown\n");
        }
    }
    let _ = writeln!(out, "Loopback capability: {}", e.loopback.label());
}

fn write_warnings(out: &mut String, warnings: &[EnumerationWarning]) {
    let total = warnings.len();
    for (i, w) in warnings.iter().enumerate() {
        out.push('\n');
        match &w.native_id {
            Some(id) => {
                let _ = writeln!(
                    out,
                    "Warning {} of {total}: endpoint {id}: {}",
                    i + 1,
                    w.message
                );
            }
            None => {
                let _ = writeln!(out, "Warning {} of {total}: {}", i + 1, w.message);
            }
        }
    }
}

/// The stable JSON document emitted by `audionet list --json`.
#[derive(Debug, Serialize)]
pub struct EndpointListDocument<'a> {
    pub schema: &'static str,
    pub schema_version: u32,
    pub backend: AudioBackend,
    pub output_count: usize,
    pub input_count: usize,
    /// In the same order as the text output.
    pub endpoints: &'a [EndpointDescriptor],
    pub warnings: &'a [EnumerationWarning],
}

impl<'a> EndpointListDocument<'a> {
    pub fn new(backend: AudioBackend, inventory: &'a EndpointInventory) -> Self {
        Self {
            schema: ENDPOINT_LIST_SCHEMA,
            schema_version: ENDPOINT_LIST_SCHEMA_VERSION,
            backend,
            output_count: inventory.count(Direction::Output),
            input_count: inventory.count(Direction::Input),
            endpoints: inventory.endpoints(),
            warnings: &inventory.warnings,
        }
    }
}

/// Renders the inventory as pretty-printed JSON with a trailing newline.
pub fn render_json(backend: AudioBackend, inventory: &EndpointInventory) -> String {
    let mut json = serde_json::to_string_pretty(&EndpointListDocument::new(backend, inventory))
        .expect("endpoint list serialization cannot fail");
    json.push('\n');
    json
}
