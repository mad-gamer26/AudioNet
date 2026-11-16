//! Another device's Sound Settings, in a window of its own (opened from
//! the device's group in Devices): each of its outputs and microphones
//! (a volume slider in 5% steps, a Mute check box, make default, default
//! for communications, disable), a phone's own volumes, and its disabled
//! devices to enable. It follows the device while open: values change in
//! place (focus stays where it is); when its controls change (a "Make
//! default" done, a device turned on), they are rebuilt with focus on the
//! same control, or the one now in its place. Standard controls only.

use std::cell::{Cell, RefCell};
use std::time::Instant;

use audionet_node::agent::Command;
use audionet_protocol::NodeId;
use audionet_protocol::signal::{ControlAction, EndpointControl, NodeSummary, SourceType};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, HBRUSH, HFONT, WHITE_BRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, TBM_SETLINESIZE, TBM_SETPAGESIZE, TBM_SETPOS, TBM_SETRANGEMAX,
    TBM_SETRANGEMIN, TBS_BOTH, TBS_HORZ, TBS_NOTICKS, TRACKBAR_CLASSW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use crate::ui::{self, wide};

const ID_CLOSE: i32 = IDCANCEL.0;
/// The trackbar's position (not in the bindings).
const TBM_GETPOS: u32 = WM_USER;
const FIRST_ID: i32 = 500;

/// One line of the window.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Note(String),
    Section(String),
    Heading(String),
    Text(String),
    Volume {
        key: String,
        target: String,
        name: String,
        value: f32,
    },
    Mute {
        key: String,
        target: String,
        name: String,
        value: bool,
    },
    Button {
        key: String,
        text: String,
        action: ControlAction,
    },
}

impl Row {
    fn key(&self) -> Option<&str> {
        match self {
            Row::Volume { key, .. } | Row::Mute { key, .. } | Row::Button { key, .. } => Some(key),
            _ => None,
        }
    }

    /// What makes the window's controls differ (not their values).
    fn shape(&self) -> String {
        match self {
            Row::Note(t) | Row::Section(t) | Row::Heading(t) | Row::Text(t) => format!("t:{t}"),
            Row::Volume { key, name, .. } | Row::Mute { key, name, .. } => format!("{key}:{name}"),
            Row::Button { key, text, .. } => format!("{key}:{text}"),
        }
    }
}

/// The rows for a device (`None`: gone).
pub fn rows(d: Option<&NodeSummary>) -> Vec<Row> {
    let Some(d) = d else {
        return vec![Row::Note("This device is no longer visible.".into())];
    };
    if !d.allows_control {
        return vec![Row::Note(format!(
            "{} doesn't let other devices change its sound settings.",
            d.name
        ))];
    }
    let mut out = Vec::new();
    let endpoint =
        |out: &mut Vec<Row>, id: &str, name: &str, is_default: bool, c: &EndpointControl| {
            let roles: Vec<&str> = [
                (is_default, "default"),
                (c.is_default_communications, "default for communications"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, r)| *r)
            .collect();
            out.push(Row::Heading(if roles.is_empty() {
                name.to_owned()
            } else {
                format!("{name} ({})", roles.join(", "))
            }));
            match c.volume {
                Some(v) if c.can_set_volume => out.push(Row::Volume {
                    key: format!("v:{id}"),
                    target: id.into(),
                    name: name.into(),
                    value: v,
                }),
                Some(v) => out.push(Row::Text(format!(
                    "Volume of {name}: {}%",
                    (v * 100.0).round()
                ))),
                None => {}
            }
            if let (true, Some(m)) = (c.can_mute, c.muted) {
                out.push(Row::Mute {
                    key: format!("m:{id}"),
                    target: id.into(),
                    name: name.into(),
                    value: m,
                });
            }
            if c.can_set_default && !is_default {
                out.push(Row::Button {
                    key: format!("d:{id}"),
                    text: format!("Make {name} the default"),
                    action: ControlAction::SetDefault {
                        target: id.into(),
                        communications: false,
                    },
                });
            }
            if c.can_set_default_communications && !c.is_default_communications {
                out.push(Row::Button {
                    key: format!("c:{id}"),
                    text: format!("Make {name} the default for communications"),
                    action: ControlAction::SetDefault {
                        target: id.into(),
                        communications: true,
                    },
                });
            }
            if c.can_enable {
                out.push(Row::Button {
                    key: format!("x:{id}"),
                    text: format!("Disable {name}"),
                    action: ControlAction::SetEnabled {
                        target: id.into(),
                        enabled: false,
                    },
                });
            }
        };
    let outputs: Vec<_> = d
        .destinations
        .iter()
        .filter(|o| o.control.is_some())
        .collect();
    let mics: Vec<_> = d
        .sources
        .iter()
        .filter(|s| s.source_type == SourceType::Input && s.control.is_some())
        .collect();
    if !outputs.is_empty() {
        out.push(Row::Section("Outputs".into()));
        for o in outputs {
            endpoint(
                &mut out,
                &o.id,
                &o.name,
                o.is_default,
                o.control.as_ref().expect("filtered"),
            );
        }
    }
    if !mics.is_empty() {
        out.push(Row::Section("Microphones".into()));
        for m in mics {
            endpoint(
                &mut out,
                &m.id,
                &m.name,
                m.is_default,
                m.control.as_ref().expect("filtered"),
            );
        }
    }
    if !d.volumes.is_empty() {
        out.push(Row::Section("Volumes".into()));
        for v in &d.volumes {
            out.push(Row::Volume {
                key: format!("v:{}", v.id),
                target: v.id.clone(),
                name: v.name.clone(),
                value: v.volume,
            });
            if let (true, Some(m)) = (v.can_mute, v.muted) {
                out.push(Row::Mute {
                    key: format!("m:{}", v.id),
                    target: v.id.clone(),
                    name: v.name.to_lowercase(),
                    value: m,
                });
            }
        }
    }
    if !d.disabled.is_empty() {
        out.push(Row::Section("Disabled".into()));
        for x in &d.disabled {
            let kind = match x.kind {
                audionet_protocol::signal::EndpointKind::Input => "microphone",
                audionet_protocol::signal::EndpointKind::Output => "output",
            };
            out.push(Row::Button {
                key: format!("e:{}", x.id),
                text: format!("Enable {} ({kind})", x.name),
                action: ControlAction::SetEnabled {
                    target: x.id.clone(),
                    enabled: true,
                },
            });
        }
    }
    if out.is_empty() {
        out.push(Row::Note(
            if d.platform == Some(audionet_protocol::Platform::Ios) {
                "An iPhone's sound settings can't be changed from other devices.".into()
            } else {
                format!(
                    "{} doesn't report its sound settings. It may need a newer AudioNet.",
                    d.name
                )
            },
        ));
    }
    out
}

struct Window {
    hwnd: HWND,
    panel: HWND,
    scale: f32,
    font: HFONT,
    account: String,
    node: NodeSummary,
    gone: bool,
    shape: Vec<String>,
    /// Each control: its row's key (None for text), window and id.
    controls: Vec<(Option<String>, HWND, i32)>,
    rows: Vec<Row>,
    scroll: i32,
    touched: Option<(String, Instant)>,
    return_focus: HWND,
}

thread_local! {
    static WIN: RefCell<Option<Window>> = const { RefCell::new(None) };
    static NEXT_ID: Cell<i32> = const { Cell::new(FIRST_ID) };
}

/// `None` when no window is open, or while it is being changed (Windows
/// can send a notification then, as a focused control is destroyed: it is
/// ignored).
fn with<T>(f: impl FnOnce(&mut Window) -> T) -> Option<T> {
    WIN.with(|w| w.try_borrow_mut().ok().and_then(|mut w| w.as_mut().map(f)))
}

/// The open window (for keyboard handling).
pub fn handle() -> Option<HWND> {
    WIN.with(|w| w.borrow().as_ref().map(|w| w.hwnd))
        .filter(|h| crate::owned::alive(*h))
}

const PANEL_H: i32 = 440;

/// Opens (or brings forward) a device's sound settings.
pub fn open(owner: HWND, account: &str, node: &NodeSummary) {
    if let Some(h) = handle() {
        let same =
            with(|w| w.account == account && w.node.node_id == node.node_id).unwrap_or(false);
        if same {
            crate::owned::bring_forward(h);
            return;
        }
        // SAFETY: closing our own window (another device's).
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
    // SAFETY: the focused window, to return to.
    let return_focus = unsafe { GetFocus() };
    let Some((hwnd, scale, font)) = crate::owned::create(
        owner,
        "SoundSettings",
        &format!("Sound Settings of {}", node.name),
        (560, PANEL_H + 64),
        Some(wndproc),
    ) else {
        return;
    };
    let s = |v: i32| (v as f32 * scale) as i32;
    let class = HSTRING::from("AudioNetSoundSettingsPanel");
    // SAFETY: registering and creating our own scrolling child window and
    // the Close button.
    let panel = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(panel_proc),
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
            &HSTRING::from("Sound settings"),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL,
            s(8),
            s(8),
            s(544),
            s(PANEL_H),
            Some(hwnd),
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default()
    };
    ui::control(
        hwnd,
        w!("BUTTON"),
        "Close",
        WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
        WINDOW_EX_STYLE(0),
        ID_CLOSE,
        (442, PANEL_H + 18, 110, 30),
        scale,
        font,
    );
    WIN.with(|w| {
        *w.borrow_mut() = Some(Window {
            hwnd,
            panel,
            scale,
            font,
            account: account.to_owned(),
            node: node.clone(),
            gone: false,
            shape: Vec::new(),
            controls: Vec::new(),
            rows: Vec::new(),
            scroll: 0,
            touched: None,
            return_focus,
        });
    });
    refresh();
    // SAFETY: showing our window, focus on its first control.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let first = with(|w| w.controls.iter().find(|c| c.0.is_some()).map(|c| c.1)).flatten();
        let _ = SetFocus(Some(first.unwrap_or_else(|| ui::item(hwnd, ID_CLOSE))));
    }
}

/// A device changed (or went: `None`) in an account.
pub fn device_changed(account: &str, node_id: &str, node: Option<&NodeSummary>) {
    let mine =
        with(|w| w.account == account && w.node.node_id.as_str() == node_id).unwrap_or(false);
    if !mine {
        return;
    }
    with(|w| match node {
        Some(n) => {
            w.node = n.clone();
            w.gone = false;
        }
        None => w.gone = true,
    });
    refresh();
}

fn new_control(w: &Window, class: PCWSTR, text: &str, style: WINDOW_STYLE) -> (HWND, i32) {
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(if id >= 2900 { FIRST_ID } else { id + 1 });
        id
    });
    // SAFETY: a standard control in our panel; the font lives as long as
    // the window.
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
            Some(w.panel),
            Some(HMENU(id as usize as *mut _)),
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default();
        SendMessageW(
            h,
            WM_SETFONT,
            Some(WPARAM(w.font.0 as usize)),
            Some(LPARAM(1)),
        );
        h
    };
    (h, id)
}

fn refresh() {
    // SAFETY: the focused window (to keep focus where it is).
    let focused = unsafe { GetFocus() };
    let rebuilt = with(|w| {
        let rows = rows((!w.gone).then_some(&w.node));
        // SAFETY: the window's title.
        unsafe {
            let _ = SetWindowTextW(w.hwnd, &wide(&format!("Sound Settings of {}", w.node.name)));
        }
        let shape: Vec<String> = rows.iter().map(Row::shape).collect();
        let rebuild = shape != w.shape;
        let mut refocus = None;
        if rebuild {
            let keyed: Vec<&(Option<String>, HWND, i32)> =
                w.controls.iter().filter(|c| c.0.is_some()).collect();
            let at = keyed.iter().position(|c| c.1 == focused);
            let key = at.and_then(|i| keyed[i].0.clone());
            for (_, h, _) in w.controls.drain(..) {
                // SAFETY: our own controls.
                unsafe {
                    let _ = DestroyWindow(h);
                }
            }
            for r in &rows {
                build_row(w, r);
            }
            w.shape = shape;
            if at.is_some() {
                let keyed: Vec<HWND> = w
                    .controls
                    .iter()
                    .filter(|c| c.0.is_some())
                    .map(|c| c.1)
                    .collect();
                let same = w.controls.iter().find(|c| c.0 == key).map(|c| c.1);
                refocus = Some(same.or_else(|| {
                    keyed
                        .get(at.unwrap_or(0).min(keyed.len().saturating_sub(1)))
                        .copied()
                }));
            }
            layout(w);
        }
        w.rows = rows.clone();
        // Values in place.
        let recent = w
            .touched
            .as_ref()
            .filter(|(_, t)| t.elapsed().as_millis() < 1000)
            .map(|(k, _)| k.clone());
        for r in &rows {
            let Some(h) = w
                .controls
                .iter()
                .find(|c| c.0.as_deref() == r.key())
                .map(|c| c.1)
            else {
                continue;
            };
            // SAFETY: standard trackbar and check box messages.
            unsafe {
                match r {
                    Row::Volume { key, value, .. } if recent.as_deref() != Some(key.as_str()) => {
                        let pos = (value * 100.0).round() as isize;
                        if SendMessageW(h, TBM_GETPOS, None, None).0 != pos {
                            SendMessageW(h, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(pos)));
                        }
                    }
                    Row::Mute { value, .. } => {
                        let want = if *value {
                            BST_CHECKED.0
                        } else {
                            BST_UNCHECKED.0
                        } as usize;
                        if SendMessageW(h, BM_GETCHECK, None, None).0 as usize != want {
                            SendMessageW(h, BM_SETCHECK, Some(WPARAM(want)), None);
                        }
                    }
                    _ => {}
                }
            }
        }
        refocus
    })
    .flatten();
    if let Some(target) = rebuilt {
        let close = handle().map(|h| ui::item(h, ID_CLOSE));
        // SAFETY: focusing our own control.
        unsafe {
            let _ = SetFocus(target.or(close));
        }
    }
}

fn build_row(w: &mut Window, r: &Row) {
    let push = |w: &mut Window,
                key: Option<String>,
                class: PCWSTR,
                text: &str,
                style: WINDOW_STYLE|
     -> HWND {
        let (h, id) = new_control(w, class, text, style);
        w.controls.push((key, h, id));
        h
    };
    match r {
        Row::Note(t) | Row::Text(t) | Row::Heading(t) | Row::Section(t) => {
            let text = if matches!(r, Row::Section(_)) {
                t.to_uppercase()
            } else {
                t.clone()
            };
            push(w, None, w!("STATIC"), &text, WINDOW_STYLE(0));
        }
        Row::Volume {
            key, name, value, ..
        } => {
            push(
                w,
                None,
                w!("STATIC"),
                &format!("Volume of {name}:"),
                WINDOW_STYLE(0),
            );
            let h = push(
                w,
                Some(key.clone()),
                TRACKBAR_CLASSW,
                "",
                WINDOW_STYLE(TBS_HORZ | TBS_BOTH | TBS_NOTICKS) | WS_TABSTOP,
            );
            // SAFETY: standard trackbar setup.
            unsafe {
                SendMessageW(h, TBM_SETRANGEMIN, Some(WPARAM(0)), Some(LPARAM(0)));
                SendMessageW(h, TBM_SETRANGEMAX, Some(WPARAM(1)), Some(LPARAM(100)));
                SendMessageW(h, TBM_SETLINESIZE, Some(WPARAM(0)), Some(LPARAM(5)));
                SendMessageW(h, TBM_SETPAGESIZE, Some(WPARAM(0)), Some(LPARAM(10)));
                SendMessageW(
                    h,
                    TBM_SETPOS,
                    Some(WPARAM(1)),
                    Some(LPARAM((value * 100.0).round() as isize)),
                );
            }
        }
        Row::Mute {
            key, name, value, ..
        } => {
            let h = push(
                w,
                Some(key.clone()),
                w!("BUTTON"),
                &format!("Mute {name}"),
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
            );
            // SAFETY: standard check box message.
            unsafe {
                SendMessageW(
                    h,
                    BM_SETCHECK,
                    Some(WPARAM(if *value {
                        BST_CHECKED.0
                    } else {
                        BST_UNCHECKED.0
                    } as usize)),
                    None,
                );
            }
        }
        Row::Button { key, text, .. } => {
            push(
                w,
                Some(key.clone()),
                w!("BUTTON"),
                text,
                WINDOW_STYLE((BS_PUSHBUTTON | BS_NOTIFY) as u32) | WS_TABSTOP,
            );
        }
    }
}

fn layout(w: &mut Window) {
    let s = |v: i32| (v as f32 * w.scale) as i32;
    let mut y = 4;
    let mut placed = Vec::new();
    for (key, h, _) in &w.controls {
        let class = {
            let mut buf = [0u16; 32];
            // SAFETY: reading a window's class name.
            let n = unsafe { GetClassNameW(*h, &mut buf) };
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        };
        let (x, width, height, gap) = match (key, class.as_str()) {
            (None, _) => (4, 500, 22, 2),
            (Some(_), c) if c.eq_ignore_ascii_case("msctls_trackbar32") => (12, 480, 32, 6),
            (Some(k), _) if k.starts_with("m:") => (12, 480, 26, 6),
            _ => (12, 420, 30, 6),
        };
        placed.push((*h, x, y, width, height));
        y += height + gap;
    }
    let content = s(y + 4);
    let view = s(PANEL_H);
    w.scroll = w.scroll.clamp(0, (content - view).max(0));
    // SAFETY: moving our own controls; z-order (tab order) = screen order.
    unsafe {
        let mut after: Option<HWND> = Some(HWND_TOP);
        for (h, x, y, width, height) in placed {
            let _ = SetWindowPos(
                h,
                after,
                s(x),
                s(y) - w.scroll,
                s(width),
                s(height),
                SWP_NOACTIVATE,
            );
            after = Some(h);
        }
        let si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: content.max(1) - 1,
            nPage: view.max(1) as u32,
            nPos: w.scroll,
            nTrackPos: 0,
        };
        windows::Win32::UI::Controls::SetScrollInfo(w.panel, SB_VERT, &si, true);
    }
}

fn scroll_to(pos: i32) {
    with(|w| {
        if pos != w.scroll {
            w.scroll = pos;
            layout(w);
        }
    });
}

/// Scrolls so `h` is in view.
fn reveal(h: HWND) {
    let Some((panel, scroll)) = with(|w| (w.panel, w.scroll)) else {
        return;
    };
    let mut r = windows::Win32::Foundation::RECT::default();
    let mut p = windows::Win32::Foundation::RECT::default();
    // SAFETY: rectangles of our own windows.
    unsafe {
        let _ = GetWindowRect(h, &mut r);
        let _ = GetWindowRect(panel, &mut p);
    }
    if r.top < p.top {
        scroll_to(scroll - (p.top - r.top) - 4);
    } else if r.bottom > p.bottom {
        scroll_to(scroll + (r.bottom - p.bottom) + 4);
    }
}

fn change(action: ControlAction) {
    let Some((account, node)) = with(|w| (w.account.clone(), w.node.node_id.clone())) else {
        return;
    };
    let request_id = format!(
        "win-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let Ok(node_id) = NodeId::new(node.as_str()) else {
        return;
    };
    let sent = ui::run_command(
        &account,
        Command::Control {
            request_id,
            node_id,
            action,
        },
    );
    if let (false, Some(h)) = (sent, handle()) {
        ui::announce(
            h,
            "The change couldn't be sent: this account isn't connected.",
        );
    }
}

fn row_for(id: i32) -> Option<(Row, HWND)> {
    with(|w| {
        let (key, h, _) = w.controls.iter().find(|c| c.2 == id)?.clone();
        let key = key?;
        w.rows
            .iter()
            .find(|r| r.key() == Some(key.as_str()))
            .map(|r| (r.clone(), h))
    })
    .flatten()
}

fn on_volume(h: HWND) {
    let Some((key, row)) = with(|w| {
        let (key, _, _) = w.controls.iter().find(|c| c.1 == h)?.clone();
        let key = key?;
        w.rows
            .iter()
            .find(|r| r.key() == Some(key.as_str()))
            .cloned()
            .map(|r| (key, r))
    })
    .flatten() else {
        return;
    };
    if let Row::Volume { target, .. } = row {
        // SAFETY: standard trackbar message.
        let pos = unsafe { SendMessageW(h, TBM_GETPOS, None, None) }.0;
        with(|w| w.touched = Some((key, Instant::now())));
        change(ControlAction::SetVolume {
            target,
            volume: (pos as f32 / 100.0).clamp(0.0, 1.0),
        });
    }
}

extern "system" fn panel_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if let Some((row, h)) = row_for(id) {
                match (row, code) {
                    (_, BN_SETFOCUS) => reveal(h),
                    (Row::Mute { target, .. }, BN_CLICKED) => {
                        // SAFETY: standard check box message.
                        let checked = unsafe { SendMessageW(h, BM_GETCHECK, None, None) }.0 as u32
                            == BST_CHECKED.0;
                        change(ControlAction::SetMuted {
                            target,
                            muted: checked,
                        });
                    }
                    (Row::Button { action, .. }, BN_CLICKED) => change(action),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        // A volume slider moved (each keyboard step, a drag).
        WM_HSCROLL => {
            let from = HWND(lparam.0 as *mut _);
            if !from.is_invalid() {
                on_volume(from);
            }
            LRESULT(0)
        }
        WM_VSCROLL => {
            if let Some((scroll, scale)) = with(|w| (w.scroll, w.scale)) {
                let s = |v: i32| (v as f32 * scale) as i32;
                let pos = match (wparam.0 & 0xFFFF) as i32 {
                    x if x == SB_LINEUP.0 => scroll - s(30),
                    x if x == SB_LINEDOWN.0 => scroll + s(30),
                    x if x == SB_PAGEUP.0 => scroll - s(PANEL_H),
                    x if x == SB_PAGEDOWN.0 => scroll + s(PANEL_H),
                    x if x == SB_THUMBTRACK.0 || x == SB_THUMBPOSITION.0 => {
                        ((wparam.0 >> 16) & 0xFFFF) as i32
                    }
                    _ => scroll,
                };
                scroll_to(pos);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(scroll) = with(|w| w.scroll) {
                scroll_to(scroll - delta / 2);
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // SAFETY: a stock brush (never freed).
            LRESULT(unsafe { GetStockObject(WHITE_BRUSH).0 } as isize)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND if (wparam.0 & 0xFFFF) as i32 == ID_CLOSE => {
            // SAFETY: closing our own window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: closing our own window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let back = WIN.with(|w| w.borrow_mut().take()).map(|w| w.return_focus);
            if let Some(back) = back.filter(|b| crate::owned::alive(*b)) {
                // SAFETY: returning focus to the button that opened us.
                unsafe {
                    let _ = SetForegroundWindow(GetAncestor(back, GA_ROOT));
                    let _ = SetFocus(Some(back));
                }
            }
            LRESULT(0)
        }
        _ => crate::owned::default(hwnd, msg, wparam, lparam),
    }
}
