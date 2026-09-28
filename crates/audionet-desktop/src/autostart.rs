//! Start at sign-in through the per-user Run key
//! (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`). No administrator
//! rights are needed and nothing is installed system-wide.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::w;

const RUN_KEY: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: windows::core::PCWSTR = w!("AudioNet");

fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" --background", exe.display()))
}

pub fn is_enabled() -> bool {
    let mut size = 0u32;
    // SAFETY: querying only the size of a REG_SZ value; all pointers valid.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    r == ERROR_SUCCESS
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        let cmd = command().ok_or("could not find this program's location")?;
        let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string; the byte count
        // includes the terminator as REG_SZ requires.
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            )
        };
        if r != ERROR_SUCCESS {
            return Err(format!(
                "Windows refused to save the setting (error {})",
                r.0
            ));
        }
    } else {
        // SAFETY: deleting a value under the current user's Run key.
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
        if r != ERROR_SUCCESS && is_enabled() {
            return Err(format!(
                "Windows refused to remove the setting (error {})",
                r.0
            ));
        }
    }
    Ok(())
}
