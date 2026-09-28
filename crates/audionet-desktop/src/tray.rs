//! The system tray (notification area) icon.
//!
//! Accessibility: the icon is reachable from the keyboard (Windows+B, then
//! arrow keys), and its tooltip is its accessible name, so it always says
//! what AudioNet is doing. Enter or a click opens the window; the
//! Applications key, Shift+F10 or a right-click opens a standard menu.
//! Notifications (`notify`) are Windows notifications, which screen
//! readers read out.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetForegroundWindow, IDI_APPLICATION, IsWindow,
    LoadIconW, MF_GRAYED, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow,
    SetMenuDefaultItem, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_APP, WM_CONTEXTMENU,
    WM_NULL,
};
use windows::core::HSTRING;

/// Callback message for tray icon events.
pub const WM_APP_TRAY: u32 = WM_APP + 10;
const ICON_ID: u32 = 1;
/// Enter or Space on the focused icon (`NIN_SELECT | NINF_KEY`).
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

pub const MENU_OPEN: u32 = 201;
pub const MENU_TOGGLE: u32 = 202;
pub const MENU_EXIT: u32 = 203;
pub const MENU_SETTINGS: u32 = 204;

/// What the user did to the icon.
pub enum TrayEvent {
    /// Click, Enter or Space: open the window.
    Open,
    /// Right-click, Applications key or Shift+F10, at these screen coordinates.
    Menu(i32, i32),
}

/// Decodes a `WM_APP_TRAY` message (`NOTIFYICON_VERSION_4` layout: the
/// event in the low word of lParam, the anchor point in wParam).
pub fn event(wparam: usize, lparam: isize) -> Option<TrayEvent> {
    let code = (lparam as u32) & 0xFFFF;
    let x = (wparam & 0xFFFF) as i16 as i32;
    let y = ((wparam >> 16) & 0xFFFF) as i16 as i32;
    match code {
        c if c == NIN_SELECT || c == NIN_KEYSELECT => Some(TrayEvent::Open),
        WM_CONTEXTMENU => Some(TrayEvent::Menu(x, y)),
        _ => None,
    }
}

fn copy_into(dst: &mut [u16], text: &str) {
    let mut n = 0;
    let room = dst.len() - 1;
    for (slot, unit) in dst.iter_mut().zip(text.encode_utf16().take(room)) {
        *slot = unit;
        n += 1;
    }
    dst[n] = 0;
}

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

/// Adds the icon (again after Explorer restarts). Returns false if the
/// shell refused.
pub fn add(hwnd: HWND, tip: &str) -> bool {
    let mut nid = base(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    // SAFETY: loading a stock system icon (shared; never destroyed).
    nid.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
    copy_into(&mut nid.szTip, tip);
    // SAFETY: `nid` is fully initialized with cbSize set and outlives the calls.
    unsafe {
        let ok = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        ok
    }
}

/// Updates the tooltip, which is also the icon's accessible name.
pub fn set_tip(hwnd: HWND, tip: &str) {
    let mut nid = base(hwnd);
    nid.uFlags = NIF_TIP | NIF_SHOWTIP;
    copy_into(&mut nid.szTip, tip);
    // SAFETY: `nid` is initialized with cbSize set and outlives the call.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// Shows a Windows notification from the icon (read by screen readers).
pub fn notify(hwnd: HWND, title: &str, text: &str, warning: bool) {
    let mut nid = base(hwnd);
    nid.uFlags = NIF_INFO;
    copy_into(&mut nid.szInfoTitle, title);
    copy_into(&mut nid.szInfo, text);
    nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
    // SAFETY: `nid` is initialized with cbSize set and outlives the call.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

pub fn remove(hwnd: HWND) {
    let nid = base(hwnd);
    // SAFETY: `nid` identifies our icon; removing a missing icon is harmless.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// Shows the icon's menu. Returns the chosen item, if any, and the window
/// that was active before (to give focus back with `restore_focus`).
pub fn menu(hwnd: HWND, x: i32, y: i32, running: bool, signed_in: bool) -> (Option<u32>, HWND) {
    // SAFETY: reading which window is active.
    let previous = unsafe { GetForegroundWindow() };
    (show_menu(hwnd, x, y, running, signed_in), previous)
}

/// Gives the foreground back to `previous` (if it still exists and is not
/// ours), so a hidden AudioNet window never keeps the focus.
pub fn restore_focus(hwnd: HWND, previous: HWND) {
    // SAFETY: checking and activating another top-level window; allowed
    // because our process is in the foreground right after the menu.
    unsafe {
        if !previous.is_invalid() && previous != hwnd && IsWindow(Some(previous)).as_bool() {
            let _ = SetForegroundWindow(previous);
        } else {
            let _ = SetFocus(None);
        }
    }
}

fn show_menu(hwnd: HWND, x: i32, y: i32, running: bool, signed_in: bool) -> Option<u32> {
    // SAFETY: a popup menu we create, show and destroy here. The foreground
    // window and WM_NULL steps are what the shell documentation requires
    // for notification-area menus to close correctly.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_OPEN as usize,
            &HSTRING::from("&Open AudioNet"),
        );
        let toggle = if running {
            "S&top sharing"
        } else {
            "&Start sharing"
        };
        let flags = if signed_in {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        };
        let _ = AppendMenuW(menu, flags, MENU_TOGGLE as usize, &HSTRING::from(toggle));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_SETTINGS as usize,
            &HSTRING::from("Setti&ngs…"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT as usize, &HSTRING::from("E&xit"));
        let _ = SetMenuDefaultItem(menu, MENU_OPEN, 0);
        let _ = SetForegroundWindow(hwnd);
        let chosen =
            TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, x, y, hwnd, None).0 as u32;
        let _ = PostMessageW(Some(hwnd), WM_NULL, Default::default(), Default::default());
        let _ = DestroyMenu(menu);
        (chosen != 0).then_some(chosen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_version_4_events() {
        let at = |x: u16, y: u16| (usize::from(y) << 16) | usize::from(x);
        assert!(matches!(
            event(0, NIN_SELECT as isize),
            Some(TrayEvent::Open)
        ));
        assert!(matches!(
            event(0, NIN_KEYSELECT as isize),
            Some(TrayEvent::Open)
        ));
        match event(at(1200, 1040), WM_CONTEXTMENU as isize | (1 << 16)) {
            Some(TrayEvent::Menu(x, y)) => assert_eq!((x, y), (1200, 1040)),
            _ => panic!("menu event"),
        }
        // Negative coordinates on a monitor left of the primary one.
        match event(at((-300i16) as u16, 20), WM_CONTEXTMENU as isize) {
            Some(TrayEvent::Menu(x, _)) => assert_eq!(x, -300),
            _ => panic!("menu event"),
        }
        assert!(event(0, 0x200).is_none()); // mouse move
    }

    #[test]
    fn tooltip_text_is_truncated_and_terminated() {
        let mut buf = [0xFFFFu16; 8];
        copy_into(&mut buf, "AudioNet: sharing");
        assert_eq!(String::from_utf16_lossy(&buf[..7]), "AudioNe");
        assert_eq!(buf[7], 0);
    }
}
