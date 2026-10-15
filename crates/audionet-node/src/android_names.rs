//! Names for Android's audio devices, and which ones to offer.
//!
//! Android lists every audio route as a device, named by the product: the
//! phone's model for everything built in ("Pixel 9", or
//! "sdk_gphone16k_x86_64" on the emulator), the accessory's own name for
//! Bluetooth and USB. cpal adds the kind it recognizes ("Pixel 9
//! (Microphone)"). Listed as they come, a phone offered several sources
//! with the same name, most of which an app may not open: the other side
//! of a phone call, the system's cast capture (remote submix), FM and TV
//! tuners.
//!
//! Here each device gets a name that says what it is, with the
//! accessory's name where it has its own, and the routes no app can use
//! are left out. Platform-neutral (no cpal types), so it is tested
//! everywhere; `cpal_audio` maps cpal's description onto [`Kind`] and
//! [`Link`].

/// What a device is (cpal's device type, reduced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Microphone,
    Speaker,
    Headphones,
    Headset,
    Earpiece,
    HearingAid,
    Dock,
    /// Not recognized (on Android: call audio, Bluetooth A2DP, USB devices,
    /// HDMI, line connections, buses), or a tuner or virtual device.
    Other,
}

/// How it is connected (cpal's interface type, reduced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    BuiltIn,
    Usb,
    Bluetooth,
    Hdmi,
    Line,
    /// The system's own capture of what plays (remote submix), and
    /// network audio: not for apps.
    Virtual,
    /// Not recognized: wired headsets and headphones, and the routes no
    /// app may open (call audio, tuners, buses).
    Other,
}

/// The name of the system's choice of microphone (listed first, the
/// default): whatever Android routes, such as a connected headset.
pub const DEFAULT_INPUT: &str = "Microphone (Android chooses)";
/// The name of the system's choice of output.
pub const DEFAULT_OUTPUT: &str = "Speaker, headphones or Bluetooth (Android chooses)";

/// The name to offer a device under, or `None` to leave it out.
/// `product` is the device's product name (cpal's name without the kind
/// it adds); `address` is Android's address for it (a built-in
/// microphone's position: "bottom", "back"; a Bluetooth device's hardware
/// address, which is never shown).
pub fn name(
    input: bool,
    kind: Kind,
    link: Link,
    product: &str,
    address: Option<&str>,
) -> Option<String> {
    let accessory = |what: &str| {
        let product = product.trim();
        if product.is_empty() {
            what.to_owned()
        } else {
            format!("{what}: {product}")
        }
    };
    let named = if input {
        match (kind, link) {
            (Kind::Microphone, Link::BuiltIn) => match position(address) {
                Some(p) => format!("Built-in microphone ({p})"),
                None => "Built-in microphone".to_owned(),
            },
            (Kind::Headset, Link::Bluetooth) => accessory("Bluetooth headset microphone"),
            (Kind::Headset, Link::Usb) => accessory("USB headset microphone"),
            (Kind::Headset, _) => "Wired headset microphone".to_owned(),
            (Kind::Microphone, Link::Usb) | (Kind::Other, Link::Usb) => {
                accessory("USB audio input")
            }
            (Kind::Other, Link::Line) => "Line input".to_owned(),
            _ => return None,
        }
    } else {
        match (kind, link) {
            (Kind::Speaker, Link::BuiltIn) => "Speaker".to_owned(),
            (Kind::Speaker, Link::Bluetooth) => accessory("Bluetooth speaker"),
            (Kind::Earpiece, _) => "Earpiece".to_owned(),
            (Kind::Headphones, _) => "Wired headphones".to_owned(),
            (Kind::Headset, Link::Bluetooth) => accessory("Bluetooth headset, call quality"),
            (Kind::Headset, Link::Usb) => accessory("USB headset"),
            (Kind::Headset, _) => "Wired headset".to_owned(),
            (Kind::HearingAid, _) => accessory("Hearing aid"),
            (Kind::Dock, _) => "Dock".to_owned(),
            (Kind::Other, Link::Bluetooth) => accessory("Bluetooth headphones or speaker"),
            (Kind::Other, Link::Usb) => accessory("USB audio output"),
            (Kind::Other, Link::Hdmi) => "HDMI".to_owned(),
            (Kind::Other, Link::Line) => "Line output".to_owned(),
            _ => return None,
        }
    };
    Some(named)
}

/// A built-in microphone's position, when Android gives one in words
/// ("bottom", "back", "top").
fn position(address: Option<&str>) -> Option<&str> {
    let a = address?.trim();
    (!a.is_empty() && a.len() <= 16 && a.chars().all(|c| c.is_ascii_alphabetic() || c == ' '))
        .then_some(a)
}

/// Makes names unique in a list ("Speaker", "Speaker 2"): Android can list
/// the same route twice (the speaker for alarms beside the speaker).
pub fn number_repeats(names: &mut [String]) {
    for i in 1..names.len() {
        let count = names[..i].iter().filter(|n| **n == names[i]).count();
        if count > 0 {
            names[i] = format!("{} {}", names[i], count + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMU: &str = "sdk_gphone16k_x86_64";

    #[test]
    fn the_emulator_offers_its_one_real_microphone() {
        // What the emulator lists as inputs (Android's names in comments).
        let inputs = [
            (Kind::Microphone, Link::BuiltIn, Some("bottom")), // Built-In Mic
            (Kind::Other, Link::Other, None),                  // Telephony Rx
            (Kind::Other, Link::Virtual, Some("0")),           // Remote Submix In
            (Kind::Other, Link::Other, None),                  // FM Tuner (a tuner)
        ];
        let offered: Vec<String> = inputs
            .iter()
            .filter_map(|&(k, l, a)| name(true, k, l, EMU, a))
            .collect();
        assert_eq!(offered, ["Built-in microphone (bottom)"]);
    }

    #[test]
    fn microphones_say_what_they_are() {
        assert_eq!(
            name(
                true,
                Kind::Microphone,
                Link::BuiltIn,
                "Pixel 9",
                Some("back")
            )
            .unwrap(),
            "Built-in microphone (back)"
        );
        assert_eq!(
            name(true, Kind::Microphone, Link::BuiltIn, "Pixel 9", Some("")).unwrap(),
            "Built-in microphone"
        );
        assert_eq!(
            name(
                true,
                Kind::Headset,
                Link::Bluetooth,
                "AirPods Pro",
                Some("AA:BB:CC:DD:EE:FF")
            )
            .unwrap(),
            "Bluetooth headset microphone: AirPods Pro",
            "the hardware address is never shown"
        );
        assert_eq!(
            name(true, Kind::Headset, Link::Other, "Pixel 9", None).unwrap(),
            "Wired headset microphone"
        );
        assert_eq!(
            name(true, Kind::Other, Link::Usb, "Scarlett Solo", None).unwrap(),
            "USB audio input: Scarlett Solo"
        );
    }

    #[test]
    fn outputs_say_what_they_are() {
        assert_eq!(
            name(false, Kind::Speaker, Link::BuiltIn, EMU, None).unwrap(),
            "Speaker"
        );
        assert_eq!(
            name(false, Kind::Earpiece, Link::BuiltIn, EMU, None).unwrap(),
            "Earpiece"
        );
        assert_eq!(
            name(false, Kind::Other, Link::Bluetooth, "AirPods Pro", None).unwrap(),
            "Bluetooth headphones or speaker: AirPods Pro"
        );
        assert_eq!(
            name(false, Kind::Other, Link::Other, EMU, None),
            None,
            "call audio"
        );
        assert_eq!(
            name(false, Kind::Other, Link::Virtual, EMU, None),
            None,
            "remote submix"
        );
    }

    #[test]
    fn repeated_names_are_numbered() {
        let mut names = vec![
            "Speaker".to_owned(),
            "Earpiece".to_owned(),
            "Speaker".to_owned(),
        ];
        number_repeats(&mut names);
        assert_eq!(names, ["Speaker", "Earpiece", "Speaker 2"]);
    }
}
