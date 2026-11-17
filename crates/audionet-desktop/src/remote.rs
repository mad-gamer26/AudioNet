//! The remote panel: this account's other visible devices, listening to
//! them and sending to them (the Devices tab), and the streams this computer
//! started (the Streams tab).
//!
//! Devices are disclosure buttons, as on the Mac (see `devlist`): each
//! visible device is one button ("Studio PC, Windows") that opens its own
//! group of controls: what to listen to and where to play it, what to send
//! and to which of its outputs, and its Sound Settings (see `soundwin`).
//! Changes are applied in place, so what is open and focus stay put.
//! Starts, stops and failures of streams are announced once; devices
//! becoming visible or no longer visible are only logged.

use std::cell::RefCell;

use audionet_node::agent::{AppEvent, Command};
use audionet_node::session::LocalMedia;
use audionet_protocol::signal::{
    DestinationInfo, NodeSummary, SessionMedia, SessionState, SourceInfo,
};
use audionet_protocol::{NodeId, Platform, SessionId};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TBM_SETLINESIZE, TBM_SETPAGESIZE,
    TBM_SETPOS, TBM_SETRANGEMAX, TBM_SETRANGEMIN, TBS_BOTH, TBS_HORZ, TBS_NOTICKS, TRACKBAR_CLASSW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::HSTRING;

use crate::ui::{Ctl, add, announce, enable, item, log};

const ID_DEVICES: i32 = 201;
const ID_STREAMS: i32 = 213;
const ID_STOP_STREAM: i32 = 214;
const ID_MUTE: i32 = 215;
const ID_VOLUME: i32 = 217;

/// The volume trackbar's steps (percent): arrow keys move by the line size,
/// Page Up and Page Down by the page size.
const VOLUME_LINE: usize = 5;
const VOLUME_PAGE: usize = 10;
/// TBM_GETPOS (WM_USER + 0), which the windows crate does not define.
const TBM_GETPOS: u32 = WM_USER;

/// A stream this computer started.
struct Stream {
    id: SessionId,
    /// The account (this computer's device id in it) the stream runs in.
    account: String,
    title: String,
    state: String,
    /// Volume (percent) and mute on this computer: what the stream plays
    /// here, or what it sends from here.
    volume: u32,
    muted: bool,
}

/// A stream's line in the list: its state, and its volume when changed.
fn stream_row(s: &Stream) -> String {
    let mut row = format!("{}: {}", s.title, s.state);
    if s.volume != 100 {
        row.push_str(&format!(", volume {} percent", s.volume));
    }
    if s.muted {
        row.push_str(", muted");
    }
    row
}

/// One account this computer is signed in to, as the panel shows it.
struct AccountView {
    /// This computer's device id in the account.
    id: String,
    /// "mad-gamer26 on audionet.example.com".
    name: String,
    online: bool,
    /// The account's other devices.
    devices: Vec<NodeSummary>,
}

#[derive(Default)]
struct Remote {
    accounts: Vec<AccountView>,
    local_sources: Vec<SourceInfo>,
    local_outputs: Vec<DestinationInfo>,
    streams: Vec<Stream>,
}

thread_local! {
    static REMOTE: RefCell<Remote> = RefCell::new(Remote::default());
}

fn with<T>(f: impl FnOnce(&mut Remote) -> T) -> T {
    REMOTE.with(|r| f(&mut r.borrow_mut()))
}

/// Sets the accounts this computer is signed in to (device id, name), in
/// order; keeps what is known about the ones that stay.
pub fn set_accounts(hwnd: HWND, accounts: Vec<(String, String)>) {
    with(|r| {
        let mut old = std::mem::take(&mut r.accounts);
        r.accounts = accounts
            .into_iter()
            .map(|(id, name)| match old.iter().position(|a| a.id == id) {
                Some(i) => {
                    let mut a = old.remove(i);
                    a.name = name;
                    a
                }
                None => AccountView {
                    id,
                    name,
                    online: false,
                    devices: Vec::new(),
                },
            })
            .collect();
        let ids: Vec<String> = r.accounts.iter().map(|a| a.id.clone()).collect();
        r.streams.retain(|s| ids.contains(&s.account));
    });
    refresh(hwnd);
}

/// Whether a control id belongs to this panel.
pub fn owns(id: i32) -> bool {
    (ID_DEVICES..=ID_VOLUME).contains(&id) || crate::devlist::owns(id)
}

/// The Devices tab's controls (labels are the control's id minus one).
pub const DEVICES_PAGE: [i32; 2] = [ID_DEVICES - 1, ID_DEVICES];
/// The Streams tab's controls.
pub const STREAMS_PAGE: [i32; 6] = [
    ID_STREAMS - 1,
    ID_STREAMS,
    ID_STOP_STREAM,
    ID_MUTE,
    ID_VOLUME - 1,
    ID_VOLUME,
];

/// The control focus goes to when the Devices or Streams tab opens.
pub const DEVICES_FIRST: i32 = ID_DEVICES;
pub const STREAMS_FIRST: i32 = ID_STREAMS;

/// Creates the Devices tab's controls and the Streams tab's, both at
/// (`x`, `y`), `w` wide (the tabs show one or the other).
pub fn build(hwnd: HWND, x: i32, y: i32, w: i32, scale: f32, font: HFONT) {
    let label = |text: &str, id: i32, y: i32| {
        add(hwnd, Ctl::label(text, id), (x, y, w, 20), scale, font);
    };
    // SAFETY: registering the common-controls class the volume needs.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
    }
    // ── Devices ──
    label(
        "De&vices (press one to open its controls):",
        ID_DEVICES - 1,
        y,
    );
    crate::devlist::create(hwnd, (x, y + 22, w, 446), scale, font);
    // ── Streams ──
    label("Streams this computer &started:", ID_STREAMS - 1, y);
    add(
        hwnd,
        Ctl::listbox(ID_STREAMS),
        (x, y + 22, w, 200),
        scale,
        font,
    );
    add(
        hwnd,
        Ctl::button("Sto&p the selected stream", ID_STOP_STREAM),
        (x, y + 232, 240, 30),
        scale,
        font,
    );
    add(
        hwnd,
        Ctl::checkbox("&Mute the selected stream", ID_MUTE),
        (x + 256, y + 232, w - 256, 30),
        scale,
        font,
    );
    label("Volume of the selecte&d stream:", ID_VOLUME - 1, y + 274);
    add(
        hwnd,
        Ctl::custom(
            TRACKBAR_CLASSW,
            WINDOW_STYLE(TBS_HORZ | TBS_BOTH | TBS_NOTICKS) | WS_TABSTOP,
            WINDOW_EX_STYLE(0),
            ID_VOLUME,
        ),
        (x, y + 296, w, 32),
        scale,
        font,
    );
    send(hwnd, TBM_SETRANGEMIN, ID_VOLUME, 0, 0);
    send(hwnd, TBM_SETRANGEMAX, ID_VOLUME, 1, 100);
    send(hwnd, TBM_SETLINESIZE, ID_VOLUME, 0, VOLUME_LINE as isize);
    send(hwnd, TBM_SETPAGESIZE, ID_VOLUME, 0, VOLUME_PAGE as isize);
    send(hwnd, TBM_SETPOS, ID_VOLUME, 1, 100);
    refresh(hwnd);
}

/// How many streams this computer started (for the Streams tab's title).
pub fn stream_count() -> usize {
    with(|r| r.streams.len())
}

/// Shows the selected stream's mute and volume.
fn sync_volume(hwnd: HWND) {
    let current = selected(hwnd, ID_STREAMS, false)
        .and_then(|i| with(|r| r.streams.get(i).map(|s| (s.volume, s.muted))));
    let (volume, muted) = current.unwrap_or((100, false));
    crate::ui::set_checked(hwnd, ID_MUTE, muted);
    send(hwnd, TBM_SETPOS, ID_VOLUME, 1, volume as isize);
}

/// Applies the mute switch and the volume slider to the selected stream.
fn apply_volume(hwnd: HWND, run: &dyn Fn(&str, Command) -> bool) {
    let Some(i) = selected(hwnd, ID_STREAMS, false) else {
        return;
    };
    let volume = send(hwnd, TBM_GETPOS, ID_VOLUME, 0, 0).clamp(0, 100) as u32;
    let muted = crate::ui::is_checked(hwnd, ID_MUTE);
    let changed = with(|r| {
        let s = r.streams.get_mut(i)?;
        if s.volume == volume && s.muted == muted {
            return None;
        }
        s.volume = volume;
        s.muted = muted;
        Some((s.id.clone(), s.account.clone(), stream_row(s)))
    });
    if let Some((id, account, row)) = changed {
        run(
            &account,
            Command::SetVolume {
                session_id: id,
                volume: volume as f32 / 100.0,
                muted,
            },
        );
        set_row(hwnd, ID_STREAMS, i, &row);
    }
}

/// A message from the volume trackbar (WM_HSCROLL): applies its position.
pub fn on_scroll(hwnd: HWND, from: HWND, run: &dyn Fn(&str, Command) -> bool) -> bool {
    if from != item(hwnd, ID_VOLUME) {
        return false;
    }
    apply_volume(hwnd, run);
    true
}

fn send(hwnd: HWND, msg: u32, id: i32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: a standard control message to one of our own controls;
    // pointers passed in `lparam` outlive the call.
    unsafe {
        SendMessageW(
            item(hwnd, id),
            msg,
            Some(WPARAM(wparam)),
            Some(LPARAM(lparam)),
        )
        .0
    }
}

fn fill(hwnd: HWND, id: i32, rows: &[String], select: Option<usize>, combo: bool) {
    let (reset, add, set) = if combo {
        (CB_RESETCONTENT, CB_ADDSTRING, CB_SETCURSEL)
    } else {
        (LB_RESETCONTENT, LB_ADDSTRING, LB_SETCURSEL)
    };
    send(hwnd, reset, id, 0, 0);
    for row in rows {
        let text = HSTRING::from(row.as_str());
        send(hwnd, add, id, 0, text.as_ptr() as isize);
    }
    if let Some(i) = select.filter(|i| *i < rows.len()) {
        send(hwnd, set, id, i, 0);
    }
}

fn selected(hwnd: HWND, id: i32, combo: bool) -> Option<usize> {
    let msg = if combo { CB_GETCURSEL } else { LB_GETCURSEL };
    usize::try_from(send(hwnd, msg, id, 0, 0)).ok()
}

/// Rewrites a list row in place (keeps the selection and the screen
/// reader's position).
fn set_row(hwnd: HWND, id: i32, index: usize, text: &str) {
    let keep = selected(hwnd, id, false);
    send(hwnd, LB_DELETESTRING, id, index, 0);
    let t = HSTRING::from(text);
    send(hwnd, LB_INSERTSTRING, id, index, t.as_ptr() as isize);
    if let Some(i) = keep {
        send(hwnd, LB_SETCURSEL, id, i, 0);
    }
}

pub(crate) fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Windows => "Windows",
        Platform::MacOs => "Mac",
        Platform::Linux => "Linux",
        Platform::Ios => "iPhone",
        Platform::Android => "Android",
        Platform::Browser => "web browser",
    }
}

// ─── the devices ────────────────────────────────────────────────────────────

/// Brings the devices panel in line with the device list, in place. With several
/// accounts, each account is a top-level item holding its devices.
fn sync_tree(_hwnd: HWND) {
    let (accounts, sources, outputs) = with(|r| {
        (
            r.accounts
                .iter()
                .map(|a| (a.id.clone(), a.name.clone(), a.online, a.devices.clone()))
                .collect::<Vec<_>>(),
            r.local_sources.clone(),
            r.local_outputs.clone(),
        )
    });
    crate::devlist::sync(&accounts, &sources, &outputs);
}

/// Refills everything but the tree (which changes in place).
fn refresh(hwnd: HWND) {
    sync_tree(hwnd);
    let streams: Vec<String> = with(|r| r.streams.iter().map(stream_row).collect());
    // Keep the selected stream selected (its volume controls follow it).
    let keep = selected(hwnd, ID_STREAMS, false)
        .filter(|i| *i < streams.len())
        .or((!streams.is_empty()).then_some(streams.len().saturating_sub(1)));
    fill(hwnd, ID_STREAMS, &streams, keep, false);
    sync_volume(hwnd);
    update_buttons(hwnd);
    crate::ui::streams_changed(hwnd);
}

/// This computer's audio devices changed (added, removed, renamed, a new
/// default): reads them again and refills "Play it on" and "Send from this
/// computer" if they differ, keeping what was chosen there when it is
/// still listed (otherwise the default).
pub fn local_devices_changed(hwnd: HWND) {
    let Ok((sources, outputs)) = crate::app::local_endpoints() else {
        return;
    };
    let changed = with(|r| {
        let changed = r.local_sources != sources || r.local_outputs != outputs;
        r.local_sources = sources;
        r.local_outputs = outputs;
        changed
    });
    if changed {
        sync_tree(hwnd);
    }
}

/// The panel's controls are always enabled: a disabled control drops out of
/// the Tab order, and one disabled while it has focus leaves focus nowhere
/// (a screen reader then says nothing). Listen, Send, Stop and the volume
/// say in words why they cannot act yet (nothing chosen, not connected).
fn update_buttons(hwnd: HWND) {
    for id in [ID_STOP_STREAM, ID_MUTE, ID_VOLUME] {
        enable(hwnd, id, true);
    }
}

/// Handles a click on one of this panel's controls.
pub fn on_command(hwnd: HWND, id: i32, code: u32, run: &dyn Fn(&str, Command) -> bool) {
    if let Some(action) = crate::devlist::on_command(id, code) {
        act(hwnd, action, run);
        return;
    }
    match (id, code) {
        (ID_STREAMS, LBN_SELCHANGE) => sync_volume(hwnd),
        (ID_MUTE, BN_CLICKED) => apply_volume(hwnd, run),
        (ID_STOP_STREAM, BN_CLICKED) => {
            let Some(i) = selected(hwnd, ID_STREAMS, false) else {
                announce(hwnd, "Select a stream first.");
                return;
            };
            if let Some((id, account)) =
                with(|r| r.streams.get(i).map(|s| (s.id.clone(), s.account.clone())))
            {
                run(&account, Command::Stop { session_id: id });
            }
        }
        _ => {}
    }
}

/// Listen, Send or Sound Settings, pressed in a device's group.
fn act(hwnd: HWND, action: crate::devlist::Action, run: &dyn Fn(&str, Command) -> bool) {
    use crate::devlist::Action;
    match action {
        Action::Say(text) => announce(hwnd, &text),
        Action::Listen {
            account,
            node,
            source,
            output,
        } => {
            let title = format!(
                "Listening to {} on {}, playing on {}",
                source.name, node.name, output.name
            );
            start(
                hwnd,
                run,
                &account,
                node.node_id.as_str(),
                SessionMedia::Listen {
                    source_id: source.id,
                },
                LocalMedia::Receive {
                    destination_id: output.id,
                },
                title,
            );
        }
        Action::Send {
            account,
            node,
            source,
            output,
        } => {
            let title = format!(
                "Sending {} to {} on {}",
                source.name, output.name, node.name
            );
            start(
                hwnd,
                run,
                &account,
                node.node_id.as_str(),
                SessionMedia::Speak {
                    destination_id: output.id,
                },
                LocalMedia::Send {
                    source_id: source.id,
                },
                title,
            );
        }
        Action::SoundSettings { account, node } => crate::soundwin::open(hwnd, &account, &node),
    }
}

/// A session id unique to this run (ids only need to be unique).
fn new_session_id() -> SessionId {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    SessionId::new(format!("app-{nanos:x}-{}", NEXT.fetch_add(1, Relaxed))).expect("valid id")
}

fn start(
    hwnd: HWND,
    run: &dyn Fn(&str, Command) -> bool,
    account: &str,
    node_id: &str,
    remote: SessionMedia,
    local: LocalMedia,
    title: String,
) {
    let Ok(node_id) = NodeId::new(node_id) else {
        return;
    };
    let session_id = new_session_id();
    let sent = run(
        account,
        Command::Start {
            session_id: session_id.clone(),
            node_id,
            remote,
            local,
        },
    );
    if !sent {
        announce(
            hwnd,
            "This account is not connected yet. Try again in a moment.",
        );
        return;
    }
    announce(hwnd, &format!("Starting: {title}."));
    log(hwnd, &format!("Starting: {title}."));
    with(|r| {
        r.streams.push(Stream {
            id: session_id,
            account: account.to_owned(),
            title,
            state: "starting".into(),
            volume: 100,
            muted: false,
        })
    });
    refresh(hwnd);
}

/// Handles an event from one account's agent (on the window thread).
pub fn on_event(hwnd: HWND, account: &str, event: AppEvent) {
    match event {
        AppEvent::Connected { .. } => {
            let local = crate::app::local_endpoints().unwrap_or_default();
            with(|r| {
                if let Some(a) = r.accounts.iter_mut().find(|a| a.id == account) {
                    a.online = true;
                }
                r.local_sources = local.0;
                r.local_outputs = local.1;
            });
            refresh(hwnd);
        }
        AppEvent::Disconnected { .. } => {
            with(|r| {
                if let Some(a) = r.accounts.iter_mut().find(|a| a.id == account) {
                    a.online = false;
                }
            });
            update_buttons(hwnd);
        }
        AppEvent::Devices(nodes) => {
            with(|r| {
                if let Some(a) = r.accounts.iter_mut().find(|a| a.id == account) {
                    // Not this computer itself.
                    a.devices = nodes
                        .into_iter()
                        .filter(|n| n.node_id.as_str() != account)
                        .collect();
                }
            });
            sync_tree(hwnd);
            update_buttons(hwnd);
        }
        AppEvent::DeviceUpdate(node) => {
            if node.node_id.as_str() == account {
                return;
            }
            crate::soundwin::device_changed(account, node.node_id.as_str(), Some(&node));
            // A visible device appeared (announced) or changed.
            let appeared = with(|r| {
                let a = r.accounts.iter_mut().find(|a| a.id == account)?;
                Some(
                    match a.devices.iter().position(|d| d.node_id == node.node_id) {
                        Some(i) => {
                            a.devices[i] = node.clone();
                            false
                        }
                        None => {
                            a.devices.push(node.clone());
                            true
                        }
                    },
                )
            })
            .unwrap_or(false);
            sync_tree(hwnd);
            // Logged, never announced (devices come and go in bursts).
            if appeared {
                log(hwnd, &format!("{} is now visible.", node.name));
            }
            update_buttons(hwnd);
        }
        AppEvent::DeviceGone { node_id } => {
            crate::soundwin::device_changed(account, node_id.as_str(), None);
            // No longer visible (visibility off, or disconnected).
            let gone = with(|r| {
                let a = r.accounts.iter_mut().find(|a| a.id == account)?;
                let i = a.devices.iter().position(|d| d.node_id == node_id)?;
                Some(a.devices.remove(i).name)
            });
            if let Some(name) = gone {
                sync_tree(hwnd);
                log(hwnd, &format!("{name} is no longer visible."));
                update_buttons(hwnd);
            }
        }
        AppEvent::Session {
            session_id,
            state,
            detail,
        } => {
            let text = match state {
                SessionState::Active => "connected".to_owned(),
                _ => detail.trim_end_matches('.').to_lowercase(),
            };
            let update = with(|r| {
                let i = r.streams.iter().position(|s| s.id == session_id)?;
                let first_connect =
                    state == SessionState::Active && r.streams[i].state != "connected";
                r.streams[i].state = text.clone();
                Some((
                    i,
                    stream_row(&r.streams[i]),
                    first_connect,
                    r.streams[i].title.clone(),
                ))
            });
            if let Some((i, row, first_connect, title)) = update {
                set_row(hwnd, ID_STREAMS, i, &row);
                if first_connect {
                    announce(hwnd, &format!("{title}: connected."));
                    log(hwnd, &format!("{title}: connected."));
                }
                if detail.starts_with("Warning:") {
                    announce(hwnd, &format!("{title}: {detail}"));
                    log(hwnd, &format!("{title}: {detail}"));
                }
            }
        }
        AppEvent::SessionEnded { session_id, reason } => {
            let ended = with(|r| {
                let i = r.streams.iter().position(|s| s.id == session_id)?;
                Some(r.streams.remove(i).title)
            });
            if let Some(title) = ended {
                announce(hwnd, &format!("Stopped: {title}. {reason}"));
                log(hwnd, &format!("Stopped: {title}. {reason}"));
                refresh(hwnd);
            }
        }
        // Measurements only when "Show measurements" is on (Settings).
        AppEvent::Diagnostics { text, .. } => {
            if crate::app::MEASUREMENTS.load(std::sync::atomic::Ordering::Relaxed) {
                log(hwnd, &text);
            }
        }
        AppEvent::ServerError { message } => {
            announce(hwnd, &message);
        }
        // A change to another device's sound settings: its new state comes
        // as a device update; a refusal is said.
        AppEvent::ControlResult { error, .. } => {
            if let Some(e) = error {
                announce(hwnd, &e);
                log(hwnd, &e);
            }
        }
    }
}

/// Every agent stopped: nothing can be listened to or sent until Start.
pub fn on_stopped(hwnd: HWND) {
    with(|r| {
        for a in &mut r.accounts {
            a.online = false;
            a.devices.clear();
        }
        r.streams.clear();
    });
    refresh(hwnd);
}

/// One account's agent stopped: its devices and streams are gone.
pub fn on_account_stopped(hwnd: HWND, account: &str) {
    with(|r| {
        if let Some(a) = r.accounts.iter_mut().find(|a| a.id == account) {
            a.online = false;
            a.devices.clear();
        }
        r.streams.retain(|s| s.account != account);
    });
    refresh(hwnd);
}
