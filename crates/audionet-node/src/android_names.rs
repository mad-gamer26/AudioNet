//! Android's microphones: which ones to offer, and their names.
//!
//! An Android phone offers one "Microphone" source and one output, like
//! the iPhone: Android routes them (a headset's microphone while one is
//! connected). Which microphone that source records from can be chosen in
//! the app (`cpal_audio::choose_microphone`), from the list made here.
//!
//! Android lists every audio route as a device, named by the product: the
//! phone's model for everything built in ("Pixel 9", or
//! "sdk_gphone16k_x86_64" on the emulator), the accessory's own name for
//! Bluetooth and USB. cpal adds the kind it recognizes ("Pixel 9
//! (Microphone)"). Listed as they come, a phone offered several inputs
//! with the same name, most of which an app may not open: the other side
//! of a phone call, the system's cast capture (remote submix), FM and TV
//! tuners. Here each microphone gets a name that says what it is, and the
//! rest are left out. Platform-neutral (no cpal types), so it is tested
//! everywhere; `cpal_audio` maps cpal's description onto [`Kind`] and
//! [`Link`].

/// What a device is (cpal's device type, reduced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Microphone,
    Headset,
    /// Anything else: not recognized (call audio, USB devices, line
    /// connections), or a speaker, tuner or virtual device.
    Other,
}

/// How it is connected (cpal's interface type, reduced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    BuiltIn,
    Usb,
    Bluetooth,
    Line,
    /// Not recognized: wired headsets, and the routes no app may open
    /// (call audio, tuners, the system's capture, network audio).
    Other,
}

/// The phone's one microphone source, as other devices see it.
pub const MICROPHONE: &str = "Microphone";
/// The phone's one output (Android routes it), as on the iPhone.
pub const OUTPUT: &str = "Speaker, headphones or Bluetooth";

/// The name to offer a microphone under, or `None` to leave it out.
/// `product` is the device's product name (cpal's name without the kind
/// it adds); `address` is Android's address for it (a built-in
/// microphone's position: "bottom", "back"; a Bluetooth device's hardware
/// address, which is never shown).
pub fn name(kind: Kind, link: Link, product: &str, address: Option<&str>) -> Option<String> {
    let accessory = |what: &str| {
        let product = product.trim();
        if product.is_empty() {
            what.to_owned()
        } else {
            format!("{what}: {product}")
        }
    };
    Some(match (kind, link) {
        (Kind::Microphone, Link::BuiltIn) => match position(address) {
            Some(p) => format!("Built-in microphone ({p})"),
            None => "Built-in microphone".to_owned(),
        },
        (Kind::Headset, Link::Bluetooth) => accessory("Bluetooth headset microphone"),
        (Kind::Headset, Link::Usb) => accessory("USB headset microphone"),
        (Kind::Headset, _) => "Wired headset microphone".to_owned(),
        (Kind::Microphone | Kind::Other, Link::Usb) => accessory("USB audio input"),
        (Kind::Other, Link::Line) => "Line input".to_owned(),
        _ => return None,
    })
}

/// A built-in microphone's position, when Android gives one in words
/// ("bottom", "back", "top").
fn position(address: Option<&str>) -> Option<&str> {
    let a = address?.trim();
    (!a.is_empty() && a.len() <= 16 && a.chars().all(|c| c.is_ascii_alphabetic() || c == ' '))
        .then_some(a)
}

/// Makes names unique in a list ("Built-in microphone", "Built-in
/// microphone 2"): a phone may not say where its microphones are.
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
            (Kind::Other, Link::Other, Some("0")),             // Remote Submix In
            (Kind::Other, Link::Other, None),                  // FM Tuner
        ];
        let offered: Vec<String> = inputs
            .iter()
            .filter_map(|&(k, l, a)| name(k, l, EMU, a))
            .collect();
        assert_eq!(offered, ["Built-in microphone (bottom)"]);
    }

    #[test]
    fn microphones_say_what_they_are() {
        let n = |k, l, p, a| name(k, l, p, a).unwrap();
        assert_eq!(
            n(Kind::Microphone, Link::BuiltIn, "Pixel 9", Some("back")),
            "Built-in microphone (back)"
        );
        assert_eq!(
            n(Kind::Microphone, Link::BuiltIn, "Pixel 9", Some("")),
            "Built-in microphone"
        );
        assert_eq!(
            n(
                Kind::Headset,
                Link::Bluetooth,
                "AirPods Pro",
                Some("AA:BB:CC:DD:EE:FF")
            ),
            "Bluetooth headset microphone: AirPods Pro",
            "the hardware address is never shown"
        );
        assert_eq!(
            n(Kind::Headset, Link::Other, "Pixel 9", None),
            "Wired headset microphone"
        );
        assert_eq!(
            n(Kind::Other, Link::Usb, "Scarlett Solo", None),
            "USB audio input: Scarlett Solo"
        );
    }

    #[test]
    fn repeated_names_are_numbered() {
        let mut names = vec![
            "Built-in microphone".to_owned(),
            "Line input".to_owned(),
            "Built-in microphone".to_owned(),
        ];
        number_repeats(&mut names);
        assert_eq!(
            names,
            ["Built-in microphone", "Line input", "Built-in microphone 2"]
        );
    }
}
