//! Golden tests for `audionet list` output. These pin the exact text a
//! screen reader will read and the JSON shape scripts depend on. They do not
//! prove screen-reader accessibility; that needs testing with NVDA and JAWS.

use audionet_audio::{EndpointInventory, EnumerationWarning};
use audionet_cli::list::{render_json, render_text};
use audionet_protocol::{
    AudioBackend, DefaultRole, DeviceFormat, Direction, EndpointDescriptor, EndpointId,
    EndpointState, LoopbackSupport, SampleFormat,
};

fn fixture() -> EndpointInventory {
    let stereo_float = DeviceFormat {
        sample_rate_hz: 48_000,
        channels: 2,
        sample_format: SampleFormat::F32,
        channel_mask: Some(3),
    };
    let e = |id: &str, name: &str, direction, state, roles: Vec<DefaultRole>, format, loopback| {
        EndpointDescriptor {
            id: EndpointId::new(AudioBackend::Wasapi, id).unwrap(),
            direction,
            name: String::from(name),
            description: None,
            adapter: None,
            state,
            default_roles: roles,
            format,
            loopback,
        }
    };
    EndpointInventory::new(
        vec![
            e(
                "{0.0.1.00000000}.{mic}",
                "Microphone (USB Audio)",
                Direction::Input,
                EndpointState::Active,
                vec![DefaultRole::Console, DefaultRole::Multimedia],
                Some(stereo_float),
                LoopbackSupport::NotApplicable,
            ),
            e(
                "{0.0.0.00000000}.{hdmi}",
                "Display Audio (HDMI)",
                Direction::Output,
                EndpointState::Unplugged,
                vec![],
                None,
                LoopbackSupport::EndpointNotActive,
            ),
            e(
                "{0.0.0.00000000}.{spk}",
                "Speakers (Realtek Audio)",
                Direction::Output,
                EndpointState::Active,
                vec![DefaultRole::Console, DefaultRole::Multimedia],
                Some(stereo_float),
                LoopbackSupport::Expected,
            ),
        ],
        vec![EnumerationWarning {
            native_id: Some("{0.0.0.00000000}.{hdmi}".into()),
            message: "Could not read the device format: example".into(),
        }],
    )
}

#[test]
fn text_output_is_linear_and_consistent() {
    let expected = "\
AudioNet found 2 output devices and 1 input device.

Output device 1 of 2: Speakers (Realtek Audio)
Identifier: {0.0.0.00000000}.{spk}
Status: Active
Default device: Yes, for console, multimedia
Channels: 2
Sample rate: 48000 Hz
Sample format: 32-bit float
Loopback capability: Expected, not yet verified

Output device 2 of 2: Display Audio (HDMI)
Identifier: {0.0.0.00000000}.{hdmi}
Status: Unplugged
Default device: No
Channels: Unknown
Sample rate: Unknown
Sample format: Unknown
Loopback capability: Unavailable, endpoint is not active

Input device 1 of 1: Microphone (USB Audio)
Identifier: {0.0.1.00000000}.{mic}
Status: Active
Default device: Yes, for console, multimedia
Channels: 2
Sample rate: 48000 Hz
Sample format: 32-bit float
Loopback capability: Not applicable to input endpoints

Warning 1 of 1: endpoint {0.0.0.00000000}.{hdmi}: Could not read the device format: example
";
    assert_eq!(render_text(&fixture()), expected);
}

#[test]
fn text_output_has_no_tabs_or_escape_sequences() {
    let text = render_text(&fixture());
    assert!(!text.contains('\t'));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn empty_inventory() {
    let text = render_text(&EndpointInventory::default());
    assert_eq!(text, "AudioNet found no audio devices.\n");
}

#[test]
fn json_document_shape_is_stable() {
    let json: serde_json::Value =
        serde_json::from_str(&render_json(AudioBackend::Wasapi, &fixture())).unwrap();
    assert_eq!(json["schema"], "audionet.endpoint_list");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["backend"], "wasapi");
    assert_eq!(json["output_count"], 2);
    assert_eq!(json["input_count"], 1);

    let endpoints = json["endpoints"].as_array().unwrap();
    assert_eq!(endpoints.len(), 3);
    // Same order as the text output.
    assert_eq!(endpoints[0]["name"], "Speakers (Realtek Audio)");
    assert_eq!(endpoints[2]["direction"], "input");

    // Every endpoint has the same keys; absent values are null, not omitted.
    let keys = |v: &serde_json::Value| {
        let mut k: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    for e in endpoints {
        assert_eq!(keys(e), keys(&endpoints[0]));
    }
    assert!(endpoints[1]["format"].is_null());
    assert_eq!(endpoints[1]["default_roles"], serde_json::json!([]));
    assert_eq!(
        endpoints[0]["format"],
        serde_json::json!({
            "sample_rate_hz": 48000,
            "channels": 2,
            "sample_format": { "encoding": "float", "container_bits": 32, "valid_bits": 32 },
            "channel_mask": 3
        })
    );
    assert_eq!(
        json["warnings"][0],
        serde_json::json!({
            "native_id": "{0.0.0.00000000}.{hdmi}",
            "message": "Could not read the device format: example"
        })
    );
}
