//! Per-user app preferences in `HKCU\Software\AudioNet` (DWORD values;
//! a separate key under `AUDIONET_TEST_PROFILE`, see `profile`).
//! No administrator rights are needed and nothing is installed system-wide.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
    RegSetKeyValueW,
};
use windows::core::{PCWSTR, w};

/// Closing the window hides it to the system tray instead of exiting.
pub const CLOSE_TO_TRAY: (PCWSTR, bool) = (w!("CloseToTray"), true);
/// Start hidden in the system tray.
pub const START_IN_TRAY: (PCWSTR, bool) = (w!("StartInTray"), false);
/// Before sharing was per account (0.7 and earlier): start sharing as soon
/// as the app opened. Read once, to carry that choice over to each
/// account (see `account_sharing`); no longer offered in Settings.
pub const START_SHARING: (PCWSTR, bool) = (w!("StartSharing"), false);
/// Check for, download and install updates automatically.
pub const AUTO_UPDATE: (PCWSTR, bool) = (w!("AutoUpdate"), true);
/// Measurements (network details, streams' packet reports) in the status
/// log, for troubleshooting. Off: the log says what happens in words only.
pub const SHOW_MEASUREMENTS: (PCWSTR, bool) = (w!("ShowMeasurements"), false);
/// The version the last update installed (string). If the app still
/// reports an older version afterwards, that release is not offered again.
pub const UPDATED_TO: PCWSTR = w!("UpdatedTo");

pub fn get((name, default): (PCWSTR, bool)) -> bool {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: reading a DWORD into a correctly sized local buffer.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            crate::profile::settings_key(),
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    if r == ERROR_SUCCESS {
        value != 0
    } else {
        default
    }
}

pub fn set((name, _): (PCWSTR, bool), on: bool) -> Result<(), String> {
    let value = u32::from(on);
    // SAFETY: writing a DWORD from a local; RegSetKeyValueW creates the key
    // if it does not exist yet.
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            crate::profile::settings_key(),
            name,
            REG_DWORD.0,
            Some((&value as *const u32).cast()),
            size_of::<u32>() as u32,
        )
    };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!(
            "Windows refused to save the setting (error {})",
            r.0
        ))
    }
}

pub fn get_string(name: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 128];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: reading a REG_SZ into a local buffer whose size in bytes is
    // passed; Windows NUL-terminates within it.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            crate::profile::settings_key(),
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

pub fn set_string(name: PCWSTR, value: &str) -> Result<(), String> {
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated UTF-16 string; the byte count includes the
    // terminator as REG_SZ requires.
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            crate::profile::settings_key(),
            name,
            REG_SZ.0,
            Some(wide.as_ptr().cast()),
            (wide.len() * 2) as u32,
        )
    };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!(
            "Windows refused to save the setting (error {})",
            r.0
        ))
    }
}

/// The value name for whether this computer shares its audio in the
/// account where it is device `node_id`.
fn sharing_name(node_id: &str) -> windows::core::HSTRING {
    windows::core::HSTRING::from(format!("Sharing-{node_id}"))
}

/// Whether this computer shares its audio in that account, if it was ever
/// chosen there.
pub fn account_sharing(node_id: &str) -> Option<bool> {
    let name = sharing_name(node_id);
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: reading a DWORD into a correctly sized local buffer; the
    // name outlives the call.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            crate::profile::settings_key(),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    (r == ERROR_SUCCESS).then_some(value != 0)
}

/// Remembers whether this computer shares its audio in that account.
pub fn set_account_sharing(node_id: &str, on: bool) -> Result<(), String> {
    let name = sharing_name(node_id);
    let value = u32::from(on);
    // SAFETY: writing a DWORD from a local; the name outlives the call.
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            crate::profile::settings_key(),
            PCWSTR(name.as_ptr()),
            REG_DWORD.0,
            Some((&value as *const u32).cast()),
            size_of::<u32>() as u32,
        )
    };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!(
            "Could not save whether to share: Windows error {}",
            r.0
        ))
    }
}

/// Forgets the sharing choice of an account this computer left.
pub fn forget_account_sharing(node_id: &str) {
    let name = sharing_name(node_id);
    // SAFETY: deleting one value under our own key; the name outlives the
    // call. A missing value is fine.
    unsafe {
        let _ = windows::Win32::System::Registry::RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            crate::profile::settings_key(),
            PCWSTR(name.as_ptr()),
        );
    }
}
