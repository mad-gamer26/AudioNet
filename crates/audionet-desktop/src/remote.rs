//! The remote panel: this account's other devices, listening to them and
//! sending to them, and the streams this computer started.
//!
//! Devices are a standard TreeView (the native Windows outline): each
//! device is one collapsed item ("Studio PC, online"); expanding it shows
//! "Sounds to listen to" and "Outputs to send to", each with its items.
//! Screen readers get names, levels, positions and expanded states from
//! Windows itself. A sound is played with Listen (or Enter) on this
//! computer's "Play it on" choice; an output receives this computer's
//! "Send from" choice with Send (or Enter).
//!
//! Changes are applied to the tree in place (renamed items, added or
//! removed devices, a device's sounds and outputs), so what is expanded,
//! the selection and a screen reader's position are kept. Starts, stops
//! and devices going online or offline are announced once.

use std::cell::RefCell;

use audionet_node::agent::{AppEvent, Command};
use audionet_node::session::LocalMedia;
use audionet_protocol::signal::{
    DestinationInfo, NodeSummary, SessionMedia, SessionState, SourceInfo, SourceType,
};
use audionet_protocol::{NodeId, SessionId};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{
    HTREEITEM, ICC_BAR_CLASSES, ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
    NM_RETURN, NMHDR, TBM_SETLINESIZE, TBM_SETPAGESIZE, TBM_SETPOS, TBM_SETRANGEMAX,
    TBM_SETRANGEMIN, TBS_BOTH, TBS_HORZ, TBS_NOTICKS, TRACKBAR_CLASSW, TVE_EXPAND, TVGN_CARET,
    TVI_LAST, TVI_ROOT, TVIF_TEXT, TVINSERTSTRUCTW, TVINSERTSTRUCTW_0, TVIS_EXPANDED, TVITEMW,
    TVM_DELETEITEM, TVM_EXPAND, TVM_GETITEMSTATE, TVM_GETNEXTITEM, TVM_INSERTITEMW, TVM_SELECTITEM,
    TVM_SETITEMW, TVS_HASBUTTONS, TVS_HASLINES, TVS_LINESATROOT, TVS_SHOWSELALWAYS, WC_TREEVIEWW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PWSTR};

use crate::ui::{Ctl, add, announce, enable, item, log};

const ID_DEVICES: i32 = 201;
const ID_LOCAL_OUTPUT: i32 = 205;
const ID_LISTEN: i32 = 206;
const ID_LOCAL_SOURCE: i32 = 208;
const ID_SEND: i32 = 211;
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

/// A device's items in the tree (tree item handles).
struct TreeDevice {
    /// The account (this computer's device id in it).
    account: String,
    node_id: String,
    item: isize,
    text: String,
    /// "Sounds to listen to" and its items (one per source).
    sounds: Option<(isize, Vec<isize>)>,
    /// "Outputs to send to" and its items (one per destination).
    outputs: Option<(isize, Vec<isize>)>,
    sources: Vec<SourceInfo>,
    destinations: Vec<DestinationInfo>,
}

/// What a tree item stands for.
enum Picked {
    Device(NodeSummary),
    Group(NodeSummary),
    Sound(NodeSummary, SourceInfo),
    Output(NodeSummary, DestinationInfo),
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
    tree: Vec<TreeDevice>,
    /// With several accounts, one top-level tree item per account (its id,
    /// tree item and text); the devices are inside it.
    account_items: Vec<(String, isize, String)>,
    grouped: bool,
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

/// Whether any account's connection is up.
fn any_online(r: &Remote) -> bool {
    r.accounts.iter().any(|a| a.online)
}

/// Every account's devices, in account order, with the account.
fn all_devices(r: &Remote) -> Vec<(String, NodeSummary)> {
    r.accounts
        .iter()
        .flat_map(|a| a.devices.iter().map(|d| (a.id.clone(), d.clone())))
        .collect()
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
    (ID_DEVICES..=ID_VOLUME).contains(&id)
}

/// Creates the panel's controls in a column starting at `x`.
pub fn build(hwnd: HWND, x: i32, scale: f32, font: HFONT) {
    let w = 500;
    let label = |text: &str, id: i32, y: i32| {
        add(hwnd, Ctl::label(text, id), (x, y, w, 20), scale, font);
    };
    let combo = |id: i32, y: i32| {
        add(hwnd, Ctl::combo(id), (x, y, w, 300), scale, font);
    };
    // SAFETY: registering the common-controls class the tree view needs.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TREEVIEW_CLASSES | ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
    }
    label("Your de&vices:", ID_DEVICES - 1, 14);
    add(
        hwnd,
        Ctl::custom(
            WC_TREEVIEWW,
            WINDOW_STYLE(TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS)
                | WS_VSCROLL
                | WS_TABSTOP,
            WS_EX_CLIENTEDGE,
            ID_DEVICES,
        ),
        (x, 36, w, 280),
        scale,
        font,
    );
    label("Play it &on:", ID_LOCAL_OUTPUT - 1, 326);
    combo(ID_LOCAL_OUTPUT, 348);
    add(
        hwnd,
        Ctl::button("Lis&ten to the chosen sound", ID_LISTEN),
        (x, 382, 240, 30),
        scale,
        font,
    );
    label("Send f&rom this computer:", ID_LOCAL_SOURCE - 1, 424);
    combo(ID_LOCAL_SOURCE, 446);
    add(
        hwnd,
        Ctl::button("S&end to the chosen output", ID_SEND),
        (x, 480, 240, 30),
        scale,
        font,
    );
    label("Streams this computer started:", ID_STREAMS - 1, 522);
    add(
        hwnd,
        Ctl::listbox(ID_STREAMS),
        (x, 544, w, 100),
        scale,
        font,
    );
    add(
        hwnd,
        Ctl::button("Stop the selected stream", ID_STOP_STREAM),
        (x, 652, 220, 30),
        scale,
        font,
    );
    add(
        hwnd,
        Ctl::checkbox("&Mute the selected stream", ID_MUTE),
        (x + 240, 652, 260, 30),
        scale,
        font,
    );
    label("Volume of the selecte&d stream:", ID_VOLUME - 1, 690);
    add(
        hwnd,
        Ctl::custom(
            TRACKBAR_CLASSW,
            WINDOW_STYLE(TBS_HORZ | TBS_BOTH | TBS_NOTICKS) | WS_TABSTOP,
            WINDOW_EX_STYLE(0),
            ID_VOLUME,
        ),
        (x, 712, w, 32),
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

fn device_row(d: &NodeSummary) -> String {
    let state = if !d.online {
        "offline"
    } else if d.sharing {
        "online"
    } else {
        "online, not sharing"
    };
    format!("{}, {state}", d.name)
}

fn source_row(s: &SourceInfo) -> String {
    let kind = match s.source_type {
        SourceType::Input => "input",
        SourceType::Loopback => "what it plays",
    };
    let default = if s.is_default { ", default" } else { "" };
    format!("{} ({kind}{default})", s.name)
}

fn output_row(d: &DestinationInfo) -> String {
    let default = if d.is_default { " (default)" } else { "" };
    format!("{}{default}", d.name)
}

fn default_index<T>(items: &[T], is_default: impl Fn(&T) -> bool) -> Option<usize> {
    items
        .iter()
        .position(is_default)
        .or((!items.is_empty()).then_some(0))
}

// ─── the device tree ────────────────────────────────────────────────────────

fn tree_insert(hwnd: HWND, parent: isize, text: &str) -> isize {
    let mut wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let ins = TVINSERTSTRUCTW {
        hParent: if parent == 0 {
            TVI_ROOT
        } else {
            HTREEITEM(parent)
        },
        hInsertAfter: TVI_LAST,
        Anonymous: TVINSERTSTRUCTW_0 {
            item: TVITEMW {
                mask: TVIF_TEXT,
                pszText: PWSTR(wide.as_mut_ptr()),
                ..Default::default()
            },
        },
    };
    send(
        hwnd,
        TVM_INSERTITEMW,
        ID_DEVICES,
        0,
        &ins as *const _ as isize,
    )
}

fn tree_set_text(hwnd: HWND, handle: isize, text: &str) {
    let mut wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let it = TVITEMW {
        mask: TVIF_TEXT,
        hItem: HTREEITEM(handle),
        pszText: PWSTR(wide.as_mut_ptr()),
        ..Default::default()
    };
    send(hwnd, TVM_SETITEMW, ID_DEVICES, 0, &it as *const _ as isize);
}

fn tree_delete(hwnd: HWND, handle: isize) {
    send(hwnd, TVM_DELETEITEM, ID_DEVICES, 0, handle);
}

fn tree_expanded(hwnd: HWND, handle: isize) -> bool {
    let state = send(
        hwnd,
        TVM_GETITEMSTATE,
        ID_DEVICES,
        handle as usize,
        TVIS_EXPANDED.0 as isize,
    );
    state as u32 & TVIS_EXPANDED.0 != 0
}

fn tree_selection(hwnd: HWND) -> isize {
    send(hwnd, TVM_GETNEXTITEM, ID_DEVICES, TVGN_CARET as usize, 0)
}

fn group_text(kind: &str, n: usize) -> String {
    format!("{kind} ({n})")
}

/// Adds a device's "Sounds to listen to" and "Outputs to send to" groups
/// (none for a device with neither, so it shows no expand button).
fn add_children(hwnd: HWND, d: &mut TreeDevice) {
    if !d.sources.is_empty() {
        let g = tree_insert(
            hwnd,
            d.item,
            &group_text("Sounds to listen to", d.sources.len()),
        );
        let items = d
            .sources
            .iter()
            .map(|s| tree_insert(hwnd, g, &source_row(s)))
            .collect();
        d.sounds = Some((g, items));
    }
    if !d.destinations.is_empty() {
        let g = tree_insert(
            hwnd,
            d.item,
            &group_text("Outputs to send to", d.destinations.len()),
        );
        let items = d
            .destinations
            .iter()
            .map(|o| tree_insert(hwnd, g, &output_row(o)))
            .collect();
        d.outputs = Some((g, items));
    }
}

/// The top-level item of an account (with several accounts).
fn account_text(a: &AccountView) -> String {
    let n = a.devices.len();
    format!(
        "Devices in {} ({n} device{})",
        a.name,
        if n == 1 { "" } else { "s" }
    )
}

/// Brings the tree in line with the device list, in place. With several
/// accounts, each account is a top-level item holding its devices.
fn sync_tree(hwnd: HWND) {
    let devices = with(|r| all_devices(r));
    let grouped = with(|r| r.accounts.len() > 1);
    // Switching between one account and several rebuilds the tree once.
    if with(|r| std::mem::replace(&mut r.grouped, grouped)) != grouped {
        send(hwnd, TVM_DELETEITEM, ID_DEVICES, 0, TVI_ROOT.0);
        with(|r| {
            r.tree.clear();
            r.account_items.clear();
        });
    }
    let mut tree = with(|r| std::mem::take(&mut r.tree));
    if grouped {
        let wanted: Vec<(String, String)> = with(|r| {
            r.accounts
                .iter()
                .map(|a| (a.id.clone(), account_text(a)))
                .collect()
        });
        let mut items = with(|r| std::mem::take(&mut r.account_items));
        items.retain(|(id, h, _)| {
            let keep = wanted.iter().any(|(w, _)| w == id);
            if !keep {
                tree_delete(hwnd, *h);
                tree.retain(|t| &t.account != id);
            }
            keep
        });
        for (id, text) in &wanted {
            match items.iter_mut().find(|(i, _, _)| i == id) {
                Some(entry) => {
                    if &entry.2 != text {
                        tree_set_text(hwnd, entry.1, text);
                        entry.2.clone_from(text);
                    }
                }
                None => {
                    let h = tree_insert(hwnd, 0, text);
                    items.push((id.clone(), h, text.clone()));
                }
            }
        }
        with(|r| r.account_items = items);
    }
    // Devices that are gone.
    tree.retain(|t| {
        let keep = devices
            .iter()
            .any(|(a, d)| d.node_id.as_str() == t.node_id && a == &t.account);
        if !keep {
            tree_delete(hwnd, t.item);
        }
        keep
    });
    for (account, d) in &devices {
        let text = device_row(d);
        match tree
            .iter_mut()
            .find(|t| t.node_id == d.node_id.as_str() && &t.account == account)
        {
            Some(t) => {
                if t.text != text {
                    tree_set_text(hwnd, t.item, &text);
                    t.text = text;
                }
                if t.sources != d.sources || t.destinations != d.destinations {
                    // Rebuild its sounds and outputs, keeping what was open
                    // and, where it still exists, the selected item.
                    let was_open = tree_expanded(hwnd, t.item);
                    let open_groups = (
                        t.sounds.as_ref().is_some_and(|g| tree_expanded(hwnd, g.0)),
                        t.outputs.as_ref().is_some_and(|g| tree_expanded(hwnd, g.0)),
                    );
                    let sel = tree_selection(hwnd);
                    let picked_id = t
                        .sounds
                        .as_ref()
                        .and_then(|g| g.1.iter().position(|h| *h == sel))
                        .map(|i| t.sources[i].id.clone())
                        .or_else(|| {
                            t.outputs
                                .as_ref()
                                .and_then(|g| g.1.iter().position(|h| *h == sel))
                                .map(|i| t.destinations[i].id.clone())
                        });
                    for g in [t.sounds.take(), t.outputs.take()].into_iter().flatten() {
                        tree_delete(hwnd, g.0);
                    }
                    t.sources.clone_from(&d.sources);
                    t.destinations.clone_from(&d.destinations);
                    add_children(hwnd, t);
                    if was_open {
                        send(hwnd, TVM_EXPAND, ID_DEVICES, TVE_EXPAND.0 as usize, t.item);
                    }
                    for (open, g) in [(open_groups.0, &t.sounds), (open_groups.1, &t.outputs)] {
                        if let (true, Some(g)) = (open, g) {
                            send(hwnd, TVM_EXPAND, ID_DEVICES, TVE_EXPAND.0 as usize, g.0);
                        }
                    }
                    if let Some(id) = picked_id {
                        let again = t
                            .sources
                            .iter()
                            .position(|s| s.id == id)
                            .and_then(|i| t.sounds.as_ref().map(|g| g.1[i]))
                            .or_else(|| {
                                t.destinations
                                    .iter()
                                    .position(|o| o.id == id)
                                    .and_then(|i| t.outputs.as_ref().map(|g| g.1[i]))
                            });
                        let target = again.unwrap_or(t.item);
                        send(
                            hwnd,
                            TVM_SELECTITEM,
                            ID_DEVICES,
                            TVGN_CARET as usize,
                            target,
                        );
                    }
                }
            }
            None => {
                let parent = with(|r| {
                    r.account_items
                        .iter()
                        .find(|(id, _, _)| id == account)
                        .map_or(0, |(_, h, _)| *h)
                });
                let item = tree_insert(hwnd, parent, &text);
                if parent != 0 {
                    // An account's devices are shown (its item open).
                    send(hwnd, TVM_EXPAND, ID_DEVICES, TVE_EXPAND.0 as usize, parent);
                }
                let mut t = TreeDevice {
                    account: account.clone(),
                    node_id: d.node_id.as_str().to_owned(),
                    item,
                    text,
                    sounds: None,
                    outputs: None,
                    sources: d.sources.clone(),
                    destinations: d.destinations.clone(),
                };
                add_children(hwnd, &mut t);
                tree.push(t);
            }
        }
    }
    with(|r| r.tree = tree);
}

/// What the selected tree item stands for, and its account.
fn picked(hwnd: HWND) -> Option<(Picked, String)> {
    let sel = tree_selection(hwnd);
    if sel == 0 {
        return None;
    }
    let found = with(|r| {
        for t in &r.tree {
            let device = r
                .accounts
                .iter()
                .find(|a| a.id == t.account)?
                .devices
                .iter()
                .find(|d| d.node_id.as_str() == t.node_id)?
                .clone();
            if t.item == sel {
                return Some(Picked::Device(device));
            }
            if let Some((g, items)) = &t.sounds {
                if *g == sel {
                    return Some(Picked::Group(device));
                }
                if let Some(i) = items.iter().position(|h| *h == sel) {
                    return Some(Picked::Sound(device, t.sources[i].clone()));
                }
            }
            if let Some((g, items)) = &t.outputs {
                if *g == sel {
                    return Some(Picked::Group(device));
                }
                if let Some(i) = items.iter().position(|h| *h == sel) {
                    return Some(Picked::Output(device, t.destinations[i].clone()));
                }
            }
        }
        None
    })?;
    let account = with(|r| {
        r.tree
            .iter()
            .find(|t| {
                t.item == sel
                    || t.sounds
                        .as_ref()
                        .is_some_and(|(g, i)| *g == sel || i.contains(&sel))
                    || t.outputs
                        .as_ref()
                        .is_some_and(|(g, i)| *g == sel || i.contains(&sel))
            })
            .map(|t| t.account.clone())
    })?;
    Some((found, account))
}

/// Refills everything but the tree (which changes in place).
fn refresh(hwnd: HWND) {
    sync_tree(hwnd);
    let (sources, outputs) = with(|r| (r.local_sources.clone(), r.local_outputs.clone()));
    fill(
        hwnd,
        ID_LOCAL_SOURCE,
        &sources.iter().map(source_row).collect::<Vec<_>>(),
        default_index(&sources, |s| {
            s.is_default && s.source_type == SourceType::Input
        }),
        true,
    );
    fill(
        hwnd,
        ID_LOCAL_OUTPUT,
        &outputs.iter().map(output_row).collect::<Vec<_>>(),
        default_index(&outputs, |d| d.is_default),
        true,
    );
    let streams: Vec<String> = with(|r| r.streams.iter().map(stream_row).collect());
    // Keep the selected stream selected (its volume controls follow it).
    let keep = selected(hwnd, ID_STREAMS, false)
        .filter(|i| *i < streams.len())
        .or((!streams.is_empty()).then_some(streams.len().saturating_sub(1)));
    fill(hwnd, ID_STREAMS, &streams, keep, false);
    sync_volume(hwnd);
    update_buttons(hwnd);
}

/// Controls stay enabled while online (disabled controls drop out of the
/// Tab order); Listen and Send say in words why they cannot start.
fn update_buttons(hwnd: HWND) {
    let online = with(|r| any_online(r));
    for id in [
        ID_DEVICES,
        ID_LOCAL_OUTPUT,
        ID_LISTEN,
        ID_LOCAL_SOURCE,
        ID_SEND,
    ] {
        enable(hwnd, id, online);
    }
    let has_streams = with(|r| !r.streams.is_empty());
    for id in [ID_STOP_STREAM, ID_MUTE, ID_VOLUME] {
        enable(hwnd, id, online && has_streams);
    }
}

/// Handles a click on one of this panel's controls.
pub fn on_command(hwnd: HWND, id: i32, code: u32, run: &dyn Fn(&str, Command) -> bool) {
    match (id, code) {
        (ID_LISTEN, BN_CLICKED) => listen(hwnd, run),
        (ID_SEND, BN_CLICKED) => send_audio(hwnd, run),
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

/// Whether `h` is the device tree (whose Enter key it handles itself).
pub fn is_tree(hwnd: HWND, h: HWND) -> bool {
    h == item(hwnd, ID_DEVICES)
}

/// Handles a notification from the device tree: Enter on a sound listens
/// to it, on an output sends to it (arrow keys expand and collapse).
/// Returns true when handled.
pub fn on_notify(hwnd: HWND, header: &NMHDR, run: &dyn Fn(&str, Command) -> bool) -> bool {
    if header.idFrom != ID_DEVICES as usize || header.code != NM_RETURN {
        return false;
    }
    match picked(hwnd) {
        Some((Picked::Sound(..), _)) => listen(hwnd, run),
        Some((Picked::Output(..), _)) => send_audio(hwnd, run),
        _ => {}
    }
    true
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

fn offline(hwnd: HWND, device: &NodeSummary) -> bool {
    if device.online {
        return false;
    }
    announce(hwnd, &format!("{} is offline.", device.name));
    true
}

fn listen(hwnd: HWND, run: &dyn Fn(&str, Command) -> bool) {
    let (device, source, account) = match picked(hwnd) {
        Some((Picked::Sound(d, s), a)) => (d, s, a),
        Some((Picked::Device(d) | Picked::Group(d) | Picked::Output(d, _), _)) => {
            announce(
                hwnd,
                &format!(
                    "Choose a sound first: expand {}, then Sounds to listen to.",
                    d.name
                ),
            );
            return;
        }
        None => {
            announce(
                hwnd,
                "Choose a sound in Your devices first: expand a device, then Sounds to listen to.",
            );
            return;
        }
    };
    if offline(hwnd, &device) {
        return;
    }
    if !device.sharing {
        announce(
            hwnd,
            &format!(
                "{} is not sharing its audio, so it cannot be listened to. You can still send to it.",
                device.name
            ),
        );
        return;
    }
    let Some(output) = selected(hwnd, ID_LOCAL_OUTPUT, true)
        .and_then(|i| with(|r| r.local_outputs.get(i).cloned()))
    else {
        announce(hwnd, "Choose where to play it in Play it on.");
        return;
    };
    let title = format!(
        "Listening to {} on {}, playing on {}",
        source.name, device.name, output.name
    );
    start(
        hwnd,
        run,
        &account,
        device.node_id.as_str(),
        SessionMedia::Listen {
            source_id: source.id,
        },
        LocalMedia::Receive {
            destination_id: output.id,
        },
        title,
    );
}

fn send_audio(hwnd: HWND, run: &dyn Fn(&str, Command) -> bool) {
    let (device, output, account) = match picked(hwnd) {
        Some((Picked::Output(d, o), a)) => (d, o, a),
        Some((Picked::Device(d) | Picked::Group(d) | Picked::Sound(d, _), _)) => {
            announce(
                hwnd,
                &format!(
                    "Choose an output first: expand {}, then Outputs to send to.",
                    d.name
                ),
            );
            return;
        }
        None => {
            announce(
                hwnd,
                "Choose an output in Your devices first: expand a device, then Outputs to send to.",
            );
            return;
        }
    };
    if offline(hwnd, &device) {
        return;
    }
    if !crate::ui::account_is_sharing(&account) {
        announce(
            hwnd,
            "This computer is not sharing its audio in this account. Press Start sharing to send from it.",
        );
        return;
    }
    let Some(source) = selected(hwnd, ID_LOCAL_SOURCE, true)
        .and_then(|i| with(|r| r.local_sources.get(i).cloned()))
    else {
        announce(hwnd, "Choose what to send in Send from this computer.");
        return;
    };
    let title = format!(
        "Sending {} to {} on {}",
        source.name, output.name, device.name
    );
    start(
        hwnd,
        run,
        &account,
        device.node_id.as_str(),
        SessionMedia::Speak {
            destination_id: output.id,
        },
        LocalMedia::Send {
            source_id: source.id,
        },
        title,
    );
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
            let (online_changed, sharing_changed) = with(|r| {
                let a = r.accounts.iter_mut().find(|a| a.id == account)?;
                Some(
                    match a.devices.iter().position(|d| d.node_id == node.node_id) {
                        Some(i) => {
                            let before = &a.devices[i];
                            let changed = (
                                before.online != node.online,
                                before.online && node.online && before.sharing != node.sharing,
                            );
                            a.devices[i] = node.clone();
                            changed
                        }
                        None => {
                            a.devices.push(node.clone());
                            (true, false)
                        }
                    },
                )
            })
            .unwrap_or((false, false));
            sync_tree(hwnd);
            if online_changed {
                let state = if node.online { "online" } else { "offline" };
                announce(hwnd, &format!("{} is now {state}.", node.name));
                log(hwnd, &format!("{} is now {state}.", node.name));
            } else if sharing_changed {
                let text = format!(
                    "{} {} sharing its audio.",
                    node.name,
                    if node.sharing { "started" } else { "stopped" }
                );
                announce(hwnd, &text);
                log(hwnd, &text);
            }
            update_buttons(hwnd);
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
