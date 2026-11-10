//! The Settings tab: AudioNet's options. Changes apply at once.
//!
//! Like the rest of the window it uses standard controls only (names, roles
//! and states come from Windows), in a logical Tab order, with access keys.
//! Its controls are children of the main window, shown while the Settings
//! tab is selected (see `ui`).

use std::cell::Cell;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::HFONT;
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

/// The Settings tab's controls.
pub const PAGE: [i32; 8] = [
    ID_AUTOSTART,
    ID_CLOSE_TO_TRAY,
    ID_START_IN_TRAY,
    ID_AUTO_UPDATE,
    ID_MEASUREMENTS,
    ID_CHECK_UPDATES,
    ID_UPDATE_STATUS,
    ID_VERSION,
];
/// Where focus goes when the Settings tab opens.
pub const FIRST: i32 = ID_AUTOSTART;

thread_local! {
    /// The main window (whose children these controls are).
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

/// Whether a control id belongs to the Settings tab.
pub fn owns(id: i32) -> bool {
    PAGE.contains(&id)
}

/// Creates the Settings tab's controls at (`x`, `y`), `w` wide.
pub fn build(hwnd: HWND, x: i32, y: i32, w: i32, scale: f32, font: HFONT) {
    WINDOW.with(|c| c.set(hwnd));
    let tab = WS_TABSTOP;
    let check = WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | tab;
    let button = WINDOW_STYLE(BS_PUSHBUTTON as u32) | tab;
    let st = WINDOW_STYLE(0);
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
            (x, y + 30 * i as i32, w, 26),
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
        (x, y + 160, 230, 30),
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
        (x, y + 200, w, 40),
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
        (x, y + 244, w, 22),
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

/// Shows the latest update result on the Settings tab.
pub fn set_update_status(text: &str) {
    let h = WINDOW.with(Cell::get);
    if !h.is_invalid() {
        ui::set_text(h, ID_UPDATE_STATUS, &format!("Updates: {text}"));
    }
}

/// Handles a click on one of the Settings tab's controls.
pub fn on_command(hwnd: HWND, id: i32) {
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
        ID_CHECK_UPDATES => ui::check_for_updates(hwnd, true),
        _ => {}
    }
}
