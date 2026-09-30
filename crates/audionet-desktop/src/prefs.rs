//! The Settings window: AudioNet's options in a window of their own, like
//! the Mac app's Settings. It opens from the main window's Settings button
//! and from the tray menu. Changes apply at once; Close or Escape closes it
//! and focus returns to where it was.
//!
//! Like the main window it uses standard controls only (names, roles and
//! states come from Windows), in a logical Tab order, with access keys.
//! The main window's message loop gives it keyboard navigation
//! (`IsDialogMessageW` with `handle()`).

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui::{self, control};
use crate::{autostart, settings, update};

const ID_AUTOSTART: i32 = 301;
const ID_CLOSE_TO_TRAY: i32 = 303;
const ID_START_IN_TRAY: i32 = 304;
const ID_AUTO_UPDATE: i32 = 305;
const ID_CHECK_UPDATES: i32 = 306;
const ID_UPDATE_STATUS: i32 = 307;
const ID_VERSION: i32 = 308;
const ID_MEASUREMENTS: i32 = 309;
/// Escape sends IDCANCEL through `IsDialogMessageW`.
const ID_CLOSE: i32 = IDCANCEL.0;

thread_local! {
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static OWNER: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static FONT: Cell<HFONT> = const { Cell::new(HFONT(std::ptr::null_mut())) };
    /// Where focus was when Settings opened, to go back there.
    static RETURN_FOCUS: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

/// The Settings window, if open (for the message loop's keyboard handling).
pub fn handle() -> Option<HWND> {
    let h = WINDOW.with(Cell::get);
    crate::owned::alive(h).then_some(h)
}

/// Opens Settings (or brings it forward), owned by the main window.
pub fn open(owner: HWND) {
    if let Some(h) = handle() {
        crate::owned::bring_forward(h);
        return;
    }
    // SAFETY: reading the focused window of our own thread.
    RETURN_FOCUS
        .with(|f| f.set(unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() }));
    OWNER.with(|o| o.set(owner));
    let Some((hwnd, scale, font)) = crate::owned::create(
        owner,
        "Settings",
        "AudioNet Settings",
        (560, 330),
        Some(wndproc),
    ) else {
        return;
    };
    WINDOW.with(|w| w.set(hwnd));
    FONT.with(|f| f.set(font));
    build(hwnd, scale, font);
    // SAFETY: showing our window and focusing its first control.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(ui::item(hwnd, ID_AUTOSTART)));
    }
}

fn build(hwnd: HWND, scale: f32, font: HFONT) {
    let tab = WS_TABSTOP;
    let check = WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | tab;
    let button = WINDOW_STYLE(BS_PUSHBUTTON as u32) | tab;
    let st = WINDOW_STYLE(0);
    let (x, full) = (16, 528);
    let rows: [(&str, i32); 5] = [
        (
            "Start AudioNet &automatically when I sign in to Windows",
            ID_AUTOSTART,
        ),
        (
            "When I close the window, &keep AudioNet running in the system tray",
            ID_CLOSE_TO_TRAY,
        ),
        ("Start minimi&zed to the system tray", ID_START_IN_TRAY),
        ("Keep AudioNet up to &date automatically", ID_AUTO_UPDATE),
        (
            "Show &measurements in the status log (for troubleshooting)",
            ID_MEASUREMENTS,
        ),
    ];
    for (i, (text, id)) in rows.iter().enumerate() {
        control(
            hwnd,
            windows::core::w!("BUTTON"),
            text,
            check,
            WINDOW_EX_STYLE(0),
            *id,
            (x, 14 + 30 * i as i32, full, 26),
            scale,
            font,
        );
    }
    control(
        hwnd,
        windows::core::w!("BUTTON"),
        "C&heck for updates now",
        button,
        WINDOW_EX_STYLE(0),
        ID_CHECK_UPDATES,
        (x, 170, 220, 30),
        scale,
        font,
    );
    // Read-only text (reachable by reading, not by Tab): the last update
    // result and the version.
    control(
        hwnd,
        windows::core::w!("STATIC"),
        "",
        st,
        WINDOW_EX_STYLE(0),
        ID_UPDATE_STATUS,
        (x, 210, full, 40),
        scale,
        font,
    );
    control(
        hwnd,
        windows::core::w!("STATIC"),
        &format!("AudioNet version {}", update::current_version()),
        st,
        WINDOW_EX_STYLE(0),
        ID_VERSION,
        (x, 254, full, 22),
        scale,
        font,
    );
    control(
        hwnd,
        windows::core::w!("BUTTON"),
        "Close",
        WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | tab,
        WINDOW_EX_STYLE(0),
        ID_CLOSE,
        (x + full - 110, 286, 110, 30),
        scale,
        font,
    );

    ui::set_checked(hwnd, ID_AUTOSTART, autostart::is_enabled());
    ui::set_checked(
        hwnd,
        ID_CLOSE_TO_TRAY,
        settings::get(settings::CLOSE_TO_TRAY),
    );
    ui::set_checked(
        hwnd,
        ID_START_IN_TRAY,
        settings::get(settings::START_IN_TRAY),
    );
    ui::set_checked(hwnd, ID_AUTO_UPDATE, settings::get(settings::AUTO_UPDATE));
    ui::set_checked(
        hwnd,
        ID_MEASUREMENTS,
        settings::get(settings::SHOW_MEASUREMENTS),
    );
    if update::configured().is_none() {
        // Source builds have no update source: say so through the state.
        ui::enable(hwnd, ID_AUTO_UPDATE, false);
        ui::enable(hwnd, ID_CHECK_UPDATES, false);
        set_update_status("This copy of AudioNet does not update itself.");
    }
}

/// Shows the latest update result in Settings, if it is open.
pub fn set_update_status(text: &str) {
    if let Some(h) = handle() {
        ui::set_text(h, ID_UPDATE_STATUS, &format!("Updates: {text}"));
    }
}

fn on_command(hwnd: HWND, id: i32) {
    match id {
        ID_AUTOSTART => {
            let checked = ui::is_checked(hwnd, ID_AUTOSTART);
            if let Err(e) = autostart::set_enabled(checked) {
                ui::message(hwnd, &e, true);
                ui::set_checked(hwnd, ID_AUTOSTART, autostart::is_enabled());
            }
        }
        ID_CLOSE_TO_TRAY | ID_START_IN_TRAY | ID_AUTO_UPDATE | ID_MEASUREMENTS => {
            let setting = match id {
                ID_CLOSE_TO_TRAY => settings::CLOSE_TO_TRAY,
                ID_START_IN_TRAY => settings::START_IN_TRAY,
                ID_MEASUREMENTS => settings::SHOW_MEASUREMENTS,
                _ => settings::AUTO_UPDATE,
            };
            if let Err(e) = settings::set(setting, ui::is_checked(hwnd, id)) {
                ui::message(hwnd, &e, true);
                ui::set_checked(hwnd, id, settings::get(setting));
            }
            if id == ID_MEASUREMENTS {
                crate::app::MEASUREMENTS.store(
                    settings::get(settings::SHOW_MEASUREMENTS),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        }
        ID_CHECK_UPDATES => ui::check_for_updates(OWNER.with(Cell::get), true),
        ID_CLOSE => close(hwnd),
        _ => {}
    }
}

fn close(hwnd: HWND) {
    // SAFETY: destroying our own window (WM_DESTROY restores focus).
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            on_command(hwnd, id);
            LRESULT(0)
        }
        WM_CLOSE => {
            close(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOW.with(|w| w.set(HWND(std::ptr::null_mut())));
            let back = RETURN_FOCUS.with(Cell::get);
            let owner = OWNER.with(Cell::get);
            // SAFETY: returning focus to the control that had it, if its
            // window is still shown; releasing our font.
            unsafe {
                if !back.is_invalid()
                    && IsWindow(Some(back)).as_bool()
                    && IsWindowVisible(owner).as_bool()
                {
                    let _ = SetForegroundWindow(owner);
                    let _ = SetFocus(Some(back));
                }
                let font = FONT.with(|f| f.replace(HFONT(std::ptr::null_mut())));
                if !font.is_invalid() {
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(font.into());
                }
            }
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
