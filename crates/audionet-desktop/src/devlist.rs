//! The Devices tab: the account's visible devices as disclosure buttons, as
//! on the Mac. Each device is one button ("Studio PC, Windows") that screen
//! readers report as collapsed or expanded; pressing it (Space or Enter)
//! opens that device's own group of controls beneath it:
//!
//! * Sound to listen to, Play it on, Listen to (device)
//! * Send from this computer, To its output, Send to (device)
//! * Sound Settings of (device)…
//!
//! The controls live in a scrolling panel (`WS_EX_CONTROLPARENT`, so Tab
//! and the arrow keys move through them as through the window's own); tab
//! order is kept the same as the order on screen, and the focused control
//! is scrolled into view. Changes (devices appearing, changing or going)
//! are applied in place, so what is expanded and focus stay put.
//!
//! The expanded or collapsed state is given to screen readers with
//! Windows' dynamic annotation (`IAccPropServices`, a property server for
//! the button's state): standard buttons have no state of their own for it.
//! Narrator reads it through Windows' MSAA-to-UIA proxy.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use audionet_protocol::signal::{DestinationInfo, NodeSummary, SourceInfo, SourceType};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, HBRUSH, HFONT, WHITE_BRUSH};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    ANNO_THIS, CAccPropServices, IAccPropServer, IAccPropServer_Impl, IAccPropServices,
    NotifyWinEvent, PROPID_ACC_STATE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, GUID, HSTRING, PCWSTR, implement, w};

use crate::ui::wide;

/// The panel's own id (its tab shows and hides it).
pub const ID_PANEL: i32 = 201;
/// Posted to the panel: focus its first control (after a rebuild).
const WM_FOCUS_FIRST: u32 = WM_APP + 1;
/// Ids of the controls inside the panel: from here up.
const FIRST_ID: i32 = 3000;

/// One kind of control in a device's group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Disclosure,
    ListenSourceLabel,
    ListenSource,
    PlayOnLabel,
    PlayOn,
    Listen,
    SendFromLabel,
    SendFrom,
    ToOutputLabel,
    ToOutput,
    Send,
    SoundSettings,
}

/// What a device's group has chosen (ids), kept while the app runs.
#[derive(Clone, Debug, Default)]
pub struct Choices {
    pub listen_source: Option<String>,
    pub play_on: Option<String>,
    pub send_from: Option<String>,
    pub to_output: Option<String>,
}

struct Row {
    account: String,
    node: NodeSummary,
    expanded: Arc<AtomicBool>,
    /// The disclosure button, then (expanded) the group's controls.
    controls: Vec<(Kind, HWND, i32)>,
}

#[derive(Default)]
struct List {
    panel: HWND,
    scale: f32,
    font: HFONT,
    width: i32,
    height: i32,
    scroll: i32,
    rows: Vec<Row>,
    /// Account headings (with several accounts), and the "no devices" text.
    headings: Vec<(String, HWND)>,
    empty: HWND,
    next_id: i32,
    choices: HashMap<String, Choices>,
    local_sources: Vec<SourceInfo>,
    local_outputs: Vec<DestinationInfo>,
}

thread_local! {
    static LIST: RefCell<List> = RefCell::new(List::default());
}

fn with<T>(f: impl FnOnce(&mut List) -> T) -> T {
    LIST.with(|l| f(&mut l.borrow_mut()))
}

/// For notifications, which Windows can send while the panel is being
/// changed (a focused control destroyed sends one at once): `None` then,
/// and the notification is ignored.
fn try_with<T>(f: impl FnOnce(&mut List) -> T) -> Option<T> {
    LIST.with(|l| l.try_borrow_mut().ok().map(|mut l| f(&mut l)))
}

/// A device in a list: "Studio PC, Windows".
pub fn device_label(d: &NodeSummary) -> String {
    match d.platform.map(crate::remote::platform_name) {
        Some(p) if !p.is_empty() => format!("{}, {p}", d.name),
        _ => d.name.clone(),
    }
}

fn source_text(s: &SourceInfo) -> String {
    format!(
        "{} ({}{})",
        s.name,
        if s.source_type == SourceType::Input {
            "input"
        } else {
            "what it plays"
        },
        if s.is_default { ", default" } else { "" }
    )
}

fn output_text(o: &DestinationInfo) -> String {
    if o.is_default {
        format!("{} (default)", o.name)
    } else {
        o.name.clone()
    }
}

// ─── the expanded/collapsed state for screen readers ──────────────────────

#[implement(IAccPropServer)]
struct ExpandState {
    button: isize,
    expanded: Arc<AtomicBool>,
}

impl IAccPropServer_Impl for ExpandState_Impl {
    fn GetPropValue(
        &self,
        _: *const u8,
        _: u32,
        prop: &GUID,
        value: *mut VARIANT,
        has: *mut BOOL,
    ) -> windows::core::Result<()> {
        // SAFETY: oleacc passes valid out pointers.
        unsafe {
            if *prop != PROPID_ACC_STATE {
                *has = BOOL(0);
                return Ok(());
            }
            let focused = GetFocus().0 as isize == self.button;
            let state = 0x0010_0000 // STATE_SYSTEM_FOCUSABLE
                | if focused { STATE_SYSTEM_FOCUSED } else { 0 }
                | if self.expanded.load(Relaxed) { STATE_SYSTEM_EXPANDED } else { STATE_SYSTEM_COLLAPSED };
            *value = VARIANT::from(state as i32);
            *has = BOOL(1);
        }
        Ok(())
    }
}

/// Gives a disclosure button its expanded/collapsed state.
fn annotate(button: HWND, expanded: Arc<AtomicBool>) {
    // SAFETY: COM is initialized on the window thread (the app does at
    // startup); the server object is reference counted and released by
    // oleacc when the window goes.
    unsafe {
        let Ok(services) =
            CoCreateInstance::<_, IAccPropServices>(&CAccPropServices, None, CLSCTX_INPROC_SERVER)
        else {
            return;
        };
        let server: IAccPropServer = ExpandState {
            button: button.0 as isize,
            expanded,
        }
        .into();
        let _ = services.SetHwndPropServer(
            button,
            OBJID_CLIENT.0 as u32,
            CHILDID_SELF,
            &[PROPID_ACC_STATE],
            &server,
            ANNO_THIS,
        );
    }
}

fn state_changed(button: HWND) {
    // SAFETY: tells screen readers the button's state changed.
    unsafe {
        NotifyWinEvent(
            EVENT_OBJECT_STATECHANGE,
            button,
            OBJID_CLIENT.0,
            CHILDID_SELF as i32,
        )
    };
}

// ─── the panel ─────────────────────────────────────────────────────────────

/// Creates the panel at (`x`, `y`), `w` by `h` (unscaled).
pub fn create(parent: HWND, rect: (i32, i32, i32, i32), scale: f32, font: HFONT) {
    let s = |v: i32| (v as f32 * scale) as i32;
    let class = HSTRING::from("AudioNetDeviceList");
    // SAFETY: registering and creating our own child window class.
    let panel = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            PCWSTR(class.as_ptr()),
            &HSTRING::from("Devices"),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL,
            s(rect.0),
            s(rect.1),
            s(rect.2),
            s(rect.3),
            Some(parent),
            Some(HMENU(ID_PANEL as usize as *mut _)),
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default()
    };
    with(|l| {
        l.panel = panel;
        l.scale = scale;
        l.font = font;
        l.width = rect.2;
        l.height = rect.3;
        l.next_id = FIRST_ID;
    });
}

fn new_control(l: &mut List, class: PCWSTR, text: &str, style: WINDOW_STYLE) -> (HWND, i32) {
    let id = l.next_id;
    // Ids are reused only after tens of thousands of controls.
    l.next_id = if l.next_id >= 60_000 {
        FIRST_ID
    } else {
        l.next_id + 1
    };
    // SAFETY: a standard control as a child of the panel; the font lives
    // as long as the app.
    let h = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let h = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &wide(text),
            style | WS_CHILD | WS_VISIBLE,
            0,
            0,
            10,
            10,
            Some(l.panel),
            Some(HMENU(id as usize as *mut _)),
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default();
        SendMessageW(
            h,
            WM_SETFONT,
            Some(WPARAM(l.font.0 as usize)),
            Some(LPARAM(1)),
        );
        h
    };
    (h, id)
}

fn destroy(h: HWND) {
    // SAFETY: one of our child controls.
    unsafe {
        let _ = DestroyWindow(h);
    }
}

fn set_text(h: HWND, text: &str) {
    // SAFETY: reading and setting a control's text.
    unsafe {
        let mut buf = vec![0u16; 512];
        let n = GetWindowTextW(h, &mut buf);
        if String::from_utf16_lossy(&buf[..n.max(0) as usize]) != text {
            let _ = SetWindowTextW(h, &wide(text));
        }
    }
}

/// Fills a drop-down list, choosing `chosen` (else the default).
fn fill(h: HWND, rows: &[String], select: Option<usize>) {
    // SAFETY: standard combo box messages.
    unsafe {
        let count = SendMessageW(h, CB_GETCOUNT, None, None).0.max(0) as usize;
        let same = count == rows.len()
            && rows.iter().enumerate().all(|(i, r)| {
                let len = SendMessageW(h, CB_GETLBTEXTLEN, Some(WPARAM(i)), None)
                    .0
                    .max(0) as usize;
                let mut buf = vec![0u16; len + 1];
                SendMessageW(
                    h,
                    CB_GETLBTEXT,
                    Some(WPARAM(i)),
                    Some(LPARAM(buf.as_mut_ptr() as isize)),
                );
                String::from_utf16_lossy(&buf[..len]) == *r
            });
        if !same {
            SendMessageW(h, CB_RESETCONTENT, None, None);
            for r in rows {
                let w = wide(r);
                SendMessageW(h, CB_ADDSTRING, None, Some(LPARAM(w.as_ptr() as isize)));
            }
        }
        let want = select.map_or(-1isize, |i| i as isize);
        if SendMessageW(h, CB_GETCURSEL, None, None).0 != want {
            SendMessageW(h, CB_SETCURSEL, Some(WPARAM(want as usize)), None);
        }
    }
}

fn selected(h: HWND) -> Option<usize> {
    // SAFETY: standard combo box message.
    let i = unsafe { SendMessageW(h, CB_GETCURSEL, None, None) }.0;
    (i >= 0).then_some(i as usize)
}

/// Brings the panel in line with the accounts' devices (`accounts`: id,
/// name, devices) and this computer's sources and outputs.
pub fn sync(
    accounts: &[(String, String, bool, Vec<NodeSummary>)],
    local_sources: &[SourceInfo],
    local_outputs: &[DestinationInfo],
) {
    // SAFETY: the focused window (to keep focus somewhere if it goes).
    let focus_before = unsafe { GetFocus() };
    with(|l| {
        if l.panel.is_invalid() {
            return;
        }
        l.local_sources = local_sources.to_vec();
        l.local_outputs = local_outputs.to_vec();
        let several = accounts.len() > 1;
        // Headings: one per account with several accounts.
        let wanted: Vec<(String, String)> = if several {
            accounts
                .iter()
                .map(|(id, name, online, devices)| {
                    let state = if !online {
                        "connecting…".to_owned()
                    } else if devices.is_empty() {
                        "no visible devices".to_owned()
                    } else {
                        format!("{} visible", devices.len())
                    };
                    (id.clone(), format!("Devices in {name} ({state})"))
                })
                .collect()
        } else {
            Vec::new()
        };
        l.headings.retain(|(id, h)| {
            let keep = wanted.iter().any(|(w, _)| w == id);
            if !keep {
                destroy(*h);
            }
            keep
        });
        for (id, text) in &wanted {
            match l.headings.iter().find(|(h, _)| h == id) {
                Some((_, h)) => set_text(*h, text),
                None => {
                    let (h, _) = new_control(l, w!("STATIC"), text, WINDOW_STYLE(0));
                    l.headings.push((id.clone(), h));
                }
            }
        }
        // Devices.
        let all: Vec<(String, NodeSummary)> = accounts
            .iter()
            .flat_map(|(id, _, _, ds)| {
                let mut ds = ds.clone();
                ds.sort_by_key(|d| d.name.to_lowercase());
                ds.into_iter().map(move |d| (id.clone(), d))
            })
            .collect();
        let mut rows = std::mem::take(&mut l.rows);
        rows.retain(|r| {
            let keep = all
                .iter()
                .any(|(a, d)| *a == r.account && d.node_id == r.node.node_id);
            if !keep {
                for (_, h, _) in &r.controls {
                    destroy(*h);
                }
            }
            keep
        });
        let mut next = Vec::new();
        for (account, d) in all {
            let row = match rows
                .iter()
                .position(|r| r.account == account && r.node.node_id == d.node_id)
            {
                Some(i) => {
                    let mut r = rows.remove(i);
                    r.node = d;
                    r
                }
                None => {
                    let expanded = Arc::new(AtomicBool::new(false));
                    let (h, id) = new_control(
                        l,
                        w!("BUTTON"),
                        &device_label(&d),
                        WINDOW_STYLE((BS_PUSHBUTTON | BS_NOTIFY | BS_LEFT) as u32) | WS_TABSTOP,
                    );
                    annotate(h, Arc::clone(&expanded));
                    Row {
                        account,
                        node: d,
                        expanded,
                        controls: vec![(Kind::Disclosure, h, id)],
                    }
                }
            };
            next.push(row);
        }
        l.rows = next;
        // Each row's texts and (expanded) group.
        for i in 0..l.rows.len() {
            sync_row(l, i);
        }
        // The "no devices" text.
        let none = l.rows.is_empty();
        if none && l.empty.is_invalid() {
            let text = if accounts.iter().all(|a| !a.2) {
                "Connecting…"
            } else {
                "No other devices are visible. Devices appear here while AudioNet runs on them with visibility on."
            };
            l.empty = new_control(l, w!("STATIC"), text, WINDOW_STYLE(0)).0;
        } else if !none && !l.empty.is_invalid() {
            destroy(l.empty);
            l.empty = HWND::default();
        } else if none {
            let text = if accounts.iter().all(|a| !a.2) {
                "Connecting…"
            } else {
                "No other devices are visible. Devices appear here while AudioNet runs on them with visibility on."
            };
            set_text(l.empty, text);
        }
        layout(l);
    });
    // Focus on a control that went: the panel's first control, never
    // nowhere.
    // SAFETY: plain focus calls on our windows.
    unsafe {
        if !focus_before.is_invalid() && !IsWindow(Some(focus_before)).as_bool() {
            focus_first();
        }
    }
}

fn sync_row(l: &mut List, i: usize) {
    let d = l.rows[i].node.clone();
    let key = format!("{}/{}", l.rows[i].account, d.node_id.as_str());
    let disclosure = l.rows[i].controls[0].1;
    set_text(disclosure, &device_label(&d));
    let expanded = l.rows[i].expanded.load(Relaxed);
    let has_group = l.rows[i].controls.len() > 1;
    if expanded && !has_group {
        let combo = WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_VSCROLL | WS_TABSTOP;
        let button = WINDOW_STYLE((BS_PUSHBUTTON | BS_NOTIFY) as u32) | WS_TABSTOP;
        let st = w!("STATIC");
        let cb = w!("COMBOBOX");
        let bt = w!("BUTTON");
        let add = |l: &mut List, kind: Kind, class: PCWSTR, text: &str, style: WINDOW_STYLE| {
            let (h, id) = new_control(l, class, text, style);
            l.rows[i].controls.push((kind, h, id));
        };
        add(
            l,
            Kind::ListenSourceLabel,
            st,
            "Sound to listen to:",
            WINDOW_STYLE(0),
        );
        add(l, Kind::ListenSource, cb, "", combo);
        add(l, Kind::PlayOnLabel, st, "Play it on:", WINDOW_STYLE(0));
        add(l, Kind::PlayOn, cb, "", combo);
        add(l, Kind::Listen, bt, "", button);
        add(
            l,
            Kind::SendFromLabel,
            st,
            "Send from this computer:",
            WINDOW_STYLE(0),
        );
        add(l, Kind::SendFrom, cb, "", combo);
        add(
            l,
            Kind::ToOutputLabel,
            st,
            "To its output:",
            WINDOW_STYLE(0),
        );
        add(l, Kind::ToOutput, cb, "", combo);
        add(l, Kind::Send, bt, "", button);
        add(l, Kind::SoundSettings, bt, "", button);
    } else if !expanded && has_group {
        for (_, h, _) in l.rows[i].controls.drain(1..) {
            destroy(h);
        }
    }
    if !expanded {
        return;
    }
    let ch = l.choices.entry(key).or_default().clone();
    let pick = |id: &Option<String>, ids: Vec<(&String, bool)>| {
        id.as_ref()
            .and_then(|c| ids.iter().position(|(i, _)| *i == c))
            .or_else(|| ids.iter().position(|(_, def)| *def))
            .or((!ids.is_empty()).then_some(0))
    };
    let local_sources = l.local_sources.clone();
    let local_outputs = l.local_outputs.clone();
    for (kind, h, _) in l.rows[i].controls.clone() {
        match kind {
            Kind::ListenSource => fill(
                h,
                &d.sources.iter().map(source_text).collect::<Vec<_>>(),
                pick(
                    &ch.listen_source,
                    d.sources
                        .iter()
                        .map(|s| (&s.id, s.is_default && s.source_type == SourceType::Loopback))
                        .collect(),
                ),
            ),
            Kind::PlayOn => fill(
                h,
                &local_outputs.iter().map(output_text).collect::<Vec<_>>(),
                pick(
                    &ch.play_on,
                    local_outputs
                        .iter()
                        .map(|o| (&o.id, o.is_default))
                        .collect(),
                ),
            ),
            Kind::SendFrom => fill(
                h,
                &local_sources.iter().map(source_text).collect::<Vec<_>>(),
                pick(
                    &ch.send_from,
                    local_sources
                        .iter()
                        .map(|s| (&s.id, s.is_default && s.source_type == SourceType::Input))
                        .collect(),
                ),
            ),
            Kind::ToOutput => fill(
                h,
                &d.destinations.iter().map(output_text).collect::<Vec<_>>(),
                pick(
                    &ch.to_output,
                    d.destinations
                        .iter()
                        .map(|o| (&o.id, o.is_default))
                        .collect(),
                ),
            ),
            Kind::Listen => set_text(h, &format!("Listen to {}", d.name)),
            Kind::Send => set_text(h, &format!("Send to {}", d.name)),
            Kind::SoundSettings => set_text(h, &format!("Sound Settings of {}…", d.name)),
            _ => {}
        }
    }
}

/// Places everything top to bottom (scrolled), with tab order following.
fn layout(l: &mut List) {
    let s = |v: i32| (v as f32 * l.scale) as i32;
    let width = l.width - 24;
    let mut y = 4;
    let mut order: Vec<(HWND, i32, i32, i32, i32)> = Vec::new();
    let place =
        |order: &mut Vec<_>, h: HWND, x: i32, y: &mut i32, w: i32, height: i32, gap: i32| {
            order.push((h, x, *y, w, height));
            *y += height + gap;
        };
    if !l.empty.is_invalid() {
        place(&mut order, l.empty, 4, &mut y, width, 44, 6);
    }
    let accounts: Vec<String> = {
        let mut a: Vec<String> = l.headings.iter().map(|(id, _)| id.clone()).collect();
        if a.is_empty() {
            a.push(String::new());
        }
        a
    };
    for account in &accounts {
        if let Some((_, h)) = l.headings.iter().find(|(id, _)| id == account) {
            place(&mut order, *h, 4, &mut y, width, 22, 4);
        }
        for r in l
            .rows
            .iter()
            .filter(|r| account.is_empty() || &r.account == account)
        {
            for (kind, h, _) in &r.controls {
                match kind {
                    Kind::Disclosure => place(&mut order, *h, 4, &mut y, width, 30, 4),
                    Kind::ListenSourceLabel
                    | Kind::PlayOnLabel
                    | Kind::SendFromLabel
                    | Kind::ToOutputLabel => place(&mut order, *h, 28, &mut y, width - 24, 20, 2),
                    Kind::ListenSource | Kind::PlayOn | Kind::SendFrom | Kind::ToOutput => {
                        // Drop-down lists: the open list's height.
                        order.push((*h, 28, y, width - 24, 300));
                        y += 30 + 6;
                    }
                    Kind::Listen | Kind::Send | Kind::SoundSettings => {
                        place(&mut order, *h, 28, &mut y, 300, 30, 8)
                    }
                }
            }
        }
    }
    let content = s(y + 4);
    let view = s(l.height);
    l.scroll = l.scroll.clamp(0, (content - view).max(0));
    // SAFETY: moving our own child windows; z-order = tab order = screen
    // order (each placed after the one before).
    unsafe {
        let mut after: Option<HWND> = Some(HWND_TOP);
        for (h, x, y, w, height) in &order {
            let _ = SetWindowPos(
                *h,
                after,
                s(*x),
                s(*y) - l.scroll,
                s(*w),
                s(*height),
                SWP_NOACTIVATE,
            );
            after = Some(*h);
        }
        let si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: content.max(1) - 1,
            nPage: view.max(1) as u32,
            nPos: l.scroll,
            nTrackPos: 0,
        };
        windows::Win32::UI::Controls::SetScrollInfo(l.panel, SB_VERT, &si, true);
    }
}

fn scroll_to(l: &mut List, pos: i32) {
    if pos != l.scroll {
        l.scroll = pos;
        layout(l);
    }
}

/// Scrolls so the control `h` is in view.
fn reveal(h: HWND) {
    try_with(|l| {
        let mut r = RECT::default();
        let mut p = RECT::default();
        // SAFETY: rectangles of our own windows.
        unsafe {
            let _ = GetWindowRect(h, &mut r);
            let _ = GetWindowRect(l.panel, &mut p);
        }
        let s = |v: i32| (v as f32 * l.scale) as i32;
        // Combo boxes are sized for their open list; only their closed part counts.
        let bottom = r.top + (r.bottom - r.top).min(s(36));
        if r.top < p.top {
            scroll_to(l, l.scroll - (p.top - r.top) - s(4));
        } else if bottom > p.bottom {
            scroll_to(l, l.scroll + (bottom - p.bottom) + s(4));
        }
    });
}

/// Focus to the panel's first control (the Devices tab opening).
pub fn focus_first() {
    // During a rebuild (Windows gives the panel focus as a focused control
    // is destroyed): once it is done.
    let Some(first) = try_with(|l| {
        l.rows
            .first()
            .map(|r| r.controls[0].1)
            .or((!l.empty.is_invalid()).then_some(l.empty))
            .unwrap_or(l.panel)
    }) else {
        let panel = LIST.with(|l| l.try_borrow().map(|l| l.panel).unwrap_or_default());
        // SAFETY: posting to our own panel (handled below).
        unsafe {
            let _ = PostMessageW(Some(panel), WM_FOCUS_FIRST, WPARAM(0), LPARAM(0));
        }
        return;
    };
    if Some(first) == try_with(|l| l.panel) {
        return;
    }
    // SAFETY: focusing one of our windows.
    unsafe {
        let _ = SetFocus(Some(first));
    }
}

/// Whether a control id is one of the panel's.
pub fn owns(id: i32) -> bool {
    id >= FIRST_ID && find(id).is_some()
}

/// What a control id is.
fn find(id: i32) -> Option<(usize, Kind, HWND)> {
    try_with(|l| {
        l.rows.iter().enumerate().find_map(|(i, r)| {
            r.controls
                .iter()
                .find(|(_, _, c)| *c == id)
                .map(|(k, h, _)| (i, *k, *h))
        })
    })
    .flatten()
}

/// A Listen, Send or Sound Settings press: what to start or open.
pub enum Action {
    Listen {
        account: String,
        node: NodeSummary,
        source: SourceInfo,
        output: DestinationInfo,
    },
    Send {
        account: String,
        node: NodeSummary,
        source: SourceInfo,
        output: DestinationInfo,
    },
    SoundSettings {
        account: String,
        node: NodeSummary,
    },
    /// Something to say instead.
    Say(String),
}

fn chosen<T: Clone>(row: &Row, kind: Kind, items: &[T]) -> Option<T> {
    row.controls
        .iter()
        .find(|(k, _, _)| *k == kind)
        .and_then(|(_, h, _)| selected(*h))
        .and_then(|i| items.get(i).cloned())
}

/// Handles a control's notification in the panel; returns what to do.
pub fn on_command(id: i32, code: u32) -> Option<Action> {
    let (i, kind, h) = find(id)?;
    if code == BN_SETFOCUS || code == CBN_SETFOCUS {
        reveal(h);
        return None;
    }
    match (kind, code) {
        (Kind::Disclosure, BN_CLICKED) => {
            let expanded = with(|l| {
                let e = !l.rows[i].expanded.load(Relaxed);
                l.rows[i].expanded.store(e, Relaxed);
                sync_row(l, i);
                layout(l);
                e
            });
            state_changed(h);
            if expanded {
                // Its group in view (as far as it fits), focus staying on it.
                let last = with(|l| l.rows[i].controls.last().map(|c| c.1));
                if let Some(last) = last {
                    reveal(last);
                }
                reveal(h);
            }
            None
        }
        (Kind::ListenSource | Kind::PlayOn | Kind::SendFrom | Kind::ToOutput, CBN_SELCHANGE) => {
            with(|l| {
                let row = &l.rows[i];
                let key = format!("{}/{}", row.account, row.node.node_id.as_str());
                let local_sources = l.local_sources.clone();
                let local_outputs = l.local_outputs.clone();
                let sources = row.node.sources.clone();
                let dests = row.node.destinations.clone();
                let ls = chosen(row, Kind::ListenSource, &sources).map(|s| s.id);
                let po = chosen(row, Kind::PlayOn, &local_outputs).map(|o| o.id);
                let sf = chosen(row, Kind::SendFrom, &local_sources).map(|s| s.id);
                let to = chosen(row, Kind::ToOutput, &dests).map(|o| o.id);
                let c = l.choices.entry(key).or_default();
                c.listen_source = ls;
                c.play_on = po;
                c.send_from = sf;
                c.to_output = to;
            });
            None
        }
        (Kind::Listen, BN_CLICKED) => Some(with(|l| {
            let r = &l.rows[i];
            match (
                chosen(r, Kind::ListenSource, &r.node.sources),
                chosen(r, Kind::PlayOn, &l.local_outputs),
            ) {
                (Some(source), Some(output)) => Action::Listen {
                    account: r.account.clone(),
                    node: r.node.clone(),
                    source,
                    output,
                },
                (None, _) => Action::Say(format!("{} offers no sound to listen to.", r.node.name)),
                (_, None) => Action::Say("This computer has no output to play it on.".into()),
            }
        })),
        (Kind::Send, BN_CLICKED) => Some(with(|l| {
            let r = &l.rows[i];
            match (
                chosen(r, Kind::SendFrom, &l.local_sources),
                chosen(r, Kind::ToOutput, &r.node.destinations),
            ) {
                (Some(source), Some(output)) => Action::Send {
                    account: r.account.clone(),
                    node: r.node.clone(),
                    source,
                    output,
                },
                (None, _) => Action::Say("This computer has no sound to send.".into()),
                (_, None) => Action::Say(format!("{} has no output to send to.", r.node.name)),
            }
        })),
        (Kind::SoundSettings, BN_CLICKED) => Some(with(|l| {
            let r = &l.rows[i];
            Action::SoundSettings {
                account: r.account.clone(),
                node: r.node.clone(),
            }
        })),
        _ => None,
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // The window handles its controls' notifications (it runs the
        // engine's commands): passed up as they are.
        WM_COMMAND => {
            // SAFETY: forwarding to our parent window.
            unsafe {
                if let Ok(parent) = GetParent(hwnd) {
                    SendMessageW(parent, msg, Some(wparam), Some(lparam));
                }
            }
            LRESULT(0)
        }
        WM_VSCROLL => {
            try_with(|l| {
                let s = |v: i32| (v as f32 * l.scale) as i32;
                let page = s(l.height);
                let pos = match (wparam.0 & 0xFFFF) as i32 {
                    x if x == SB_LINEUP.0 => l.scroll - s(30),
                    x if x == SB_LINEDOWN.0 => l.scroll + s(30),
                    x if x == SB_PAGEUP.0 => l.scroll - page,
                    x if x == SB_PAGEDOWN.0 => l.scroll + page,
                    x if x == SB_THUMBTRACK.0 || x == SB_THUMBPOSITION.0 => {
                        ((wparam.0 >> 16) & 0xFFFF) as i32
                    }
                    x if x == SB_TOP.0 => 0,
                    x if x == SB_BOTTOM.0 => i32::MAX,
                    _ => l.scroll,
                };
                scroll_to(l, pos);
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            try_with(|l| {
                let s = |v: i32| (v as f32 * l.scale) as i32;
                scroll_to(l, l.scroll - delta * s(60) / 120);
            });
            LRESULT(0)
        }
        // Focus given to the panel (its tab opening) goes to its first control.
        WM_SETFOCUS | WM_FOCUS_FIRST => {
            // (Only if focus is still on the panel itself by then.)
            // SAFETY: reading the focus.
            if msg == WM_SETFOCUS || unsafe { GetFocus() } == hwnd {
                focus_first();
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // White behind labels, like the window.
            // SAFETY: a stock brush (never freed).
            LRESULT(unsafe { GetStockObject(WHITE_BRUSH).0 } as isize)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
