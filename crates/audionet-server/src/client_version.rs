//! Which client versions a server accepts (`min_client_version`).
//!
//! Every AudioNet client names itself in its `hello` (`ClientInfo::software`):
//! "audionet-desktop 1.3.0", "audionet-app 1.3.0", "audionet-node 1.3.0",
//! "audionet-nvda 1.3.0", "audionet-visitor (iPhone) 1.3.0", with the
//! version last. The web client ("audionet-web …") is exempt: this server
//! serves it, so it is always current. A client whose version cannot be
//! read is refused too when a minimum is set. The name is the client's own
//! word: this keeps out old clients, it is not a security check.

/// A version as (major, minor, patch).
pub type Version = (u32, u32, u32);

/// "1.3" or "1.3.0" (a missing patch is 0).
pub fn parse(text: &str) -> Option<Version> {
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

pub fn show((a, b, c): Version) -> String {
    format!("{a}.{b}.{c}")
}

/// The version a client names, if it can be read.
pub fn of_software(software: &str) -> Option<Version> {
    parse(software.split_whitespace().last()?)
}

/// Why this client is refused (a sentence for the person using it), or
/// `None` when it may connect.
pub fn refusal(software: &str, min: Option<Version>, download: Option<&str>) -> Option<String> {
    let min = min?;
    if software.starts_with("audionet-web ") {
        return None;
    }
    let get = match download {
        Some(url) => format!(" Download the new version from {url}."),
        None => " Update AudioNet on this device.".to_owned(),
    };
    match of_software(software) {
        Some(v) if v >= min => None,
        Some(v) => Some(format!(
            "This AudioNet is version {}, and this server needs version {} or later.{get}",
            show(v),
            show(min)
        )),
        None => Some(format!(
            "This server needs AudioNet version {} or later, and this client did not say its version.{get}",
            show(min)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse("1.3"), Some((1, 3, 0)));
        assert_eq!(parse("1.3.0"), Some((1, 3, 0)));
        assert_eq!(parse("1.12.4"), Some((1, 12, 4)));
        assert_eq!(parse("1.3.0.1"), None);
        assert_eq!(parse("1.x"), None);
        assert_eq!(parse(""), None);
        assert_eq!(of_software("audionet-desktop 1.2.5"), Some((1, 2, 5)));
        assert_eq!(
            of_software("audionet-visitor (Matthew's iPhone) 1.3.0"),
            Some((1, 3, 0))
        );
        assert_eq!(of_software("test"), None);
    }

    #[test]
    fn who_is_refused() {
        let min = Some((1, 3, 0));
        let url = Some("https://example.org/releases");
        for ok in [
            "audionet-desktop 1.3.0",
            "audionet-app 1.3.1",
            "audionet-nvda 2.0.0",
            "audionet-node 1.4.0",
            "audionet-web 0.1.0",
        ] {
            assert_eq!(refusal(ok, min, url), None, "{ok}");
        }
        let old = refusal("audionet-desktop 1.2.5", min, url).unwrap();
        assert_eq!(
            old,
            "This AudioNet is version 1.2.5, and this server needs version 1.3.0 or later. Download the new version from https://example.org/releases."
        );
        assert!(
            refusal("audionet-nvda 1.2.2", min, None)
                .unwrap()
                .ends_with("Update AudioNet on this device.")
        );
        assert!(
            refusal("something", min, url)
                .unwrap()
                .contains("did not say its version")
        );
        // No minimum: everyone.
        assert_eq!(refusal("audionet-desktop 0.1.0", None, url), None);
    }
}
