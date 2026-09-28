//! Test isolation. With `AUDIONET_TEST_PROFILE=<name>` the app uses its own
//! window class, so it neither hands off to nor is found by a copy the
//! user is running, and its own preferences key, so tests never read or
//! change the user's settings. Normal use never sets it.

use std::sync::OnceLock;

use windows::core::{HSTRING, PCWSTR};

fn test_profile() -> Option<String> {
    std::env::var("AUDIONET_TEST_PROFILE")
        .ok()
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

fn cached(cell: &'static OnceLock<HSTRING>, make: impl FnOnce() -> String) -> PCWSTR {
    PCWSTR(cell.get_or_init(|| HSTRING::from(make())).as_ptr())
}

/// The main window's class name (also how a second start finds the first).
pub fn class_name() -> PCWSTR {
    static NAME: OnceLock<HSTRING> = OnceLock::new();
    cached(&NAME, || match test_profile() {
        Some(p) => format!("AudioNetDesktop-{p}"),
        None => "AudioNetDesktop".into(),
    })
}

/// The per-user preferences key under HKEY_CURRENT_USER.
pub fn settings_key() -> PCWSTR {
    static KEY: OnceLock<HSTRING> = OnceLock::new();
    cached(&KEY, || match test_profile() {
        Some(p) => format!("Software\\AudioNet-Test-{p}"),
        None => "Software\\AudioNet".into(),
    })
}
