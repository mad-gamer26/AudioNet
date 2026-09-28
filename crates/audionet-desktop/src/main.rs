//! AudioNet desktop app (Windows).
//!
//! A small window built from standard Win32 controls (labelled edit boxes,
//! buttons, a checkbox and a read-only status log), which screen readers
//! support fully. Important status changes are announced with UI Automation
//! notifications, without moving focus.
//!
//! Closing the window keeps AudioNet running in the system tray (unless
//! that option is turned off). `audionet-desktop --background` starts in
//! the tray and connects automatically (used for starting at sign-in). The
//! "Start minimized to the system tray" and "Start sharing automatically"
//! options apply to every start. Only one copy runs: starting it again
//! shows the existing window.
//!
//! Official builds update themselves (see `update`): the running copy
//! starts the new one with `--after-update <pid> --from <version>`, plus
//! `--show` and `--share` to keep the window and sharing as they were.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod handoff;
#[cfg(windows)]
mod logwin;
#[cfg(windows)]
mod owned;
#[cfg(windows)]
mod prefs;
#[cfg(windows)]
mod profile;
#[cfg(windows)]
mod remote;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod update;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let after_update = value("--after-update")
        .and_then(|pid| pid.parse::<u32>().ok())
        .map(|old_pid| {
            // Report that the new program runs, then let the old copy exit.
            handoff::take_over(old_pid, std::time::Duration::from_secs(30));
            ui::AfterUpdate {
                from: value("--from").unwrap_or_default(),
                show: args.iter().any(|a| a == "--show"),
                share: args.iter().any(|a| a == "--share"),
            }
        });
    ui::run(ui::Launch {
        background: args.iter().any(|a| a == "--background"),
        after_update,
    });
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The AudioNet desktop app is only available for Windows so far.");
    std::process::exit(1);
}
