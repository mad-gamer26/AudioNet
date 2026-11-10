//! The window: standard Win32 controls only, so NVDA, JAWS and Narrator
//! get names, roles, states and keyboard behavior from Windows itself.
//!
//! Layout: until this computer is signed in to an account, the window shows
//! only the sign-in page. Then a standard tab control holds four tabs:
//! Devices (the account's visible devices: listen and send), Streams (the
//! streams this computer started: stop, mute, volume), Accounts (the
//! accounts, this computer's visibility in each, sign out, add another,
//! manage devices) and Settings. "Status log…" and "Exit" are always there.
//! Ctrl+Tab and Ctrl+Shift+Tab (and Ctrl+Page Down / Page Up) move between
//! tabs from anywhere in the window, as in Windows property sheets; the
//! arrow keys do on the tab control itself. Only the selected tab's
//! controls exist on screen (the others are hidden), so Tab and screen
//! readers see one page at a time.
//!
//! Accessibility details:
//! * Every edit box is preceded by its label (a STATIC control) in creation
//!   order, which is how Windows derives the accessible name. Labels carry
//!   `&` access keys (Alt+letter).
//! * Tab order is creation order; `IsDialogMessage` provides Tab, Shift+Tab,
//!   arrow keys, access keys and Enter for the default button.
//! * The status log opens in a window of its own ("Status log…", see
//!   `logwin`): a read-only multi-line edit screen-reader users move through
//!   line by line.
//! * Important changes are spoken with `UiaRaiseNotificationEvent`, which
//!   does not move focus.
//! * Sizes follow the monitor DPI; text uses the system message font.
//! * Closing the window can hide it to the system tray (see `tray`); the
//!   first time, a Windows notification says where AudioNet went and how
//!   to exit. Exit (button or tray menu) always quits.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering::Relaxed};

use audionet_node::config::NodeConfig;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetStockObject, HFONT, HGDIOBJ, UpdateWindow, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{
    NotificationKind_Other, NotificationProcessing_ImportantMostRecent, UiaHostProviderFromHwnd,
    UiaRaiseNotificationEvent,
};
use windows::Win32::UI::Controls::{
    TCIF_TEXT, TCITEMW, TCM_GETCURSEL, TCM_INSERTITEMW, TCM_SETCURSEL, TCM_SETITEMW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BSTR, HSTRING, PCWSTR, w};

use crate::tray::{self, TrayEvent};
use crate::{app, handoff, settings, update};

// Labels are the id of the control they name minus one.
const ID_SERVER: i32 = 101;
const ID_NAME: i32 = 105;
const ID_SIGN_OUT: i32 = 118;
/// The accounts this computer is signed in to (a list box).
const ID_ACCOUNTS: i32 = 112;
/// This computer's visibility in the selected account (a checkbox).
const ID_VISIBLE: i32 = 114;
/// What visibility means (read-only text).
const ID_VISIBLE_HELP: i32 = 115;
const ID_ADD_ACCOUNT: i32 = 116;
/// Opens the web client's Manage devices (rename and remove devices).
const ID_MANAGE: i32 = 117;
const ID_OPEN_WEB: i32 = 131;
const ID_LOG: i32 = 132;
const ID_EXIT: i32 = 134;
const ID_USER: i32 = 141;
const ID_PASSWORD: i32 = 143;
const ID_SIGN_IN: i32 = 144;
const ID_FORGOT: i32 = 145;
/// Back from adding another account to the tabs.
const ID_CANCEL_ADD: i32 = 146;
/// The sign-in page's heading (read-only text).
const ID_SIGN_IN_HEADING: i32 = 147;
const ID_TABS: i32 = 150;

/// The sign-in page's controls.
const SIGN_IN_PAGE: [i32; 12] = [
    ID_SIGN_IN_HEADING,
    ID_SERVER - 1,
    ID_SERVER,
    ID_USER - 1,
    ID_USER,
    ID_PASSWORD - 1,
    ID_PASSWORD,
    ID_NAME - 1,
    ID_NAME,
    ID_SIGN_IN,
    ID_FORGOT,
    ID_CANCEL_ADD,
];
/// The Accounts tab's controls.
const ACCOUNTS_PAGE: [i32; 8] = [
    ID_ACCOUNTS - 1,
    ID_ACCOUNTS,
    ID_VISIBLE,
    ID_VISIBLE_HELP,
    ID_SIGN_OUT,
    ID_ADD_ACCOUNT,
    ID_MANAGE,
    ID_OPEN_WEB,
];

/// What the window shows: the sign-in page, or one of the tabs (in the tab
/// control's order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    SignIn,
    Devices,
    Streams,
    Accounts,
    Settings,
}

const TABS: [Page; 4] = [Page::Devices, Page::Streams, Page::Accounts, Page::Settings];

impl Page {
    fn controls(self) -> &'static [i32] {
        match self {
            Page::SignIn => &SIGN_IN_PAGE,
            Page::Devices => &crate::remote::DEVICES_PAGE,
            Page::Streams => &crate::remote::STREAMS_PAGE,
            Page::Accounts => &ACCOUNTS_PAGE,
            Page::Settings => &crate::prefs::PAGE,
        }
    }

    /// Where focus goes when the page opens.
    fn first(self) -> i32 {
        match self {
            Page::SignIn => ID_SERVER,
            Page::Devices => crate::remote::DEVICES_FIRST,
            Page::Streams => crate::remote::STREAMS_FIRST,
            Page::Accounts => ID_ACCOUNTS,
            Page::Settings => crate::prefs::FIRST,
        }
    }
}

/// Update timer: runs the schedule below.
const TIMER_UPDATE: usize = 1;
const TIMER_PERIOD_MS: u32 = 15_000;
/// Audio devices changed: waits this long for more changes (one plug-in
/// brings several notifications), then reads the devices again.
const TIMER_DEVICES: usize = 2;
const DEVICES_SETTLE_MS: u32 = 500;
const FIRST_CHECK: std::time::Duration = std::time::Duration::from_secs(30);
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 3600);
const RETRY_AFTER_FAILURE: std::time::Duration = std::time::Duration::from_secs(3600);
/// How long the new copy has to report that it started.
const HANDOFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Status line from a background thread (lParam: Box<StatusMsg>).
const WM_APP_STATUS: u32 = WM_APP + 1;
/// Sign-in finished (lParam: Box<Result<NodeConfig, String>>).
const WM_APP_SIGNED_IN: u32 = WM_APP + 2;
/// Agent stopped (lParam: Box<Option<String>>).
const WM_APP_ENDED: u32 = WM_APP + 3;
/// Remote-control event from the agent (lParam: Box<AppEvent>).
const WM_APP_REMOTE: u32 = WM_APP + 5;
/// Update check finished (lParam: Box<UpdateOutcome>).
const WM_APP_UPDATE: u32 = WM_APP + 4;
/// Removing this computer from an account finished (lParam:
/// Box<(NodeConfig, Result<(), String>)>).
const WM_APP_SIGNED_OUT: u32 = WM_APP + 6;
/// This computer's audio devices changed (no lParam).
const WM_APP_DEVICES: u32 = WM_APP + 7;

/// How the app was started.
pub struct Launch {
    /// Started at Windows sign-in: in the tray.
    pub background: bool,
    pub after_update: Option<AfterUpdate>,
}

/// Started by the previous copy after installing an update.
pub struct AfterUpdate {
    pub from: String,
    pub show: bool,
}

struct UpdateOutcome {
    manual: bool,
    result: Result<Option<update::Prepared>, String>,
}

/// The update schedule and a prepared update waiting to be installed.
struct Updates {
    next_check: std::time::Instant,
    checking: bool,
    prepared: Option<update::Prepared>,
    told_waiting: bool,
}

struct StatusMsg {
    text: String,
    announce: bool,
}

struct State {
    hwnd: HWND,
    font: HFONT,
    /// Every account this computer is signed in to (it is a device in each).
    configs: Vec<NodeConfig>,
    /// The running agents, one per account (by this computer's device id
    /// there): this computer is connected to every account while AudioNet
    /// runs, visible there or not as chosen per account.
    running: Vec<(String, app::Running)>,
    /// Accounts whose agent is connected to the server now.
    connected: Vec<String>,
    /// What the Accounts list shows (rebuilt only when this changes, so
    /// moving through it is not disturbed).
    account_rows: Vec<String>,
    signing_in: bool,
    /// What the window shows now.
    page: Page,
    /// The sign-in page is open to add another account (it has Cancel).
    adding: bool,
    /// The Streams tab's title, as last set (it holds the count).
    streams_title: String,
    /// The "still running in the tray" notification was shown this run.
    tray_notice_shown: bool,
    updates: Updates,
}

/// Registered message ids, kept outside `STATE`: the window procedure
/// checks them on every message, including ones Windows sends while
/// `STATE` is borrowed (for example WM_CTLCOLORBTN during `set_text`).
/// Sent by Explorer when it (re)starts: the tray icon must be re-added.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// Sent by a second copy of the app: show this window instead.
static SHOW_REQUEST: AtomicU32 = AtomicU32::new(0);
/// The control that had focus when the window was last deactivated:
/// focus returns there when the person comes back (Alt+Tab), as in dialog
/// boxes.
static LAST_FOCUS: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

pub(crate) fn item(hwnd: HWND, id: i32) -> HWND {
    // SAFETY: plain lookup of a child control of our window.
    unsafe { GetDlgItem(Some(hwnd), id) }.unwrap_or_default()
}

pub(crate) fn set_text(hwnd: HWND, id: i32, text: &str) {
    // SAFETY: `item` is a child of our window; the string outlives the call.
    unsafe {
        let _ = SetWindowTextW(item(hwnd, id), &wide(text));
    }
}

fn get_text(hwnd: HWND, id: i32) -> String {
    let h = item(hwnd, id);
    // SAFETY: the buffer is sized from GetWindowTextLengthW plus terminator.
    unsafe {
        let len = GetWindowTextLengthW(h).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(h, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

pub(crate) fn enable(hwnd: HWND, id: i32, on: bool) {
    // SAFETY: enabling or disabling one of our child controls.
    unsafe {
        let _ = EnableWindow(item(hwnd, id), on);
    }
}

/// Speaks `text` through UI Automation without moving focus.
pub(crate) fn announce(hwnd: HWND, text: &str) {
    // From the window the person is in: Settings when it is in front.
    // SAFETY: reading the foreground window handle has no preconditions.
    let foreground = unsafe { GetForegroundWindow() };
    let hwnd = crate::logwin::handle()
        .filter(|w| *w == foreground)
        .unwrap_or(hwnd);
    // SAFETY: the host provider for our own window; BSTRs are owned here.
    unsafe {
        if let Ok(provider) = UiaHostProviderFromHwnd(hwnd) {
            let _ = UiaRaiseNotificationEvent(
                &provider,
                NotificationKind_Other,
                NotificationProcessing_ImportantMostRecent,
                &BSTR::from(text),
                &BSTR::from("AudioNetStatus"),
            );
        }
    }
}

/// Adds a line to the status log (see `logwin`).
pub(crate) fn log(_hwnd: HWND, text: &str) {
    crate::logwin::append(text);
}

fn post<T>(hwnd: isize, msg: u32, payload: T) {
    let ptr = Box::into_raw(Box::new(payload)) as isize;
    // SAFETY: the window procedure takes ownership back with Box::from_raw
    // for exactly these message ids. If posting fails, reclaim the box.
    unsafe {
        if PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(ptr)).is_err() {
            drop(Box::from_raw(ptr as *mut T));
        }
    }
}

/// Whether a status line deserves a spoken announcement.
fn important(text: &str) -> bool {
    [
        "Connected to",
        "Disconnected",
        "requested",
        "ended",
        "Connected.",
        "stopped",
        "failed",
        "Could not",
        "no longer",
        "now visible",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// The account the Visible checkbox and Sign out act on: the one selected
/// in Accounts (the only one when there is one).
fn chosen_account(hwnd: HWND, s: &State) -> Option<NodeConfig> {
    let i = list_selection(hwnd, ID_ACCOUNTS).unwrap_or(0);
    s.configs.get(i).or(s.configs.first()).cloned()
}

/// Whether this computer is visible in that account.
fn visible_in(s: &State, node_id: &str) -> bool {
    s.running
        .iter()
        .any(|(id, r)| id == node_id && r.is_visible())
}

fn refresh(hwnd: HWND) {
    let rows = with_state(|s| {
        let signed_in = !s.configs.is_empty();
        // The fields stay enabled while signing in (disabling the focused
        // one would leave focus nowhere); a second press is refused.
        set_text(
            hwnd,
            ID_SIGN_IN,
            match (s.signing_in, s.adding) {
                (true, _) => "Signing &in…",
                (false, true) => "&Add account",
                (false, false) => "Sign &in",
            },
        );
        // "Forgot password" stays enabled (so it can be found with Tab) and
        // explains when the server address is missing.
        for id in [ID_ACCOUNTS, ID_SIGN_OUT, ID_VISIBLE, ID_MANAGE] {
            enable(hwnd, id, signed_in);
        }
        // The checkbox acts on the selected account and says so when there
        // are several.
        let chosen_visible = chosen_account(hwnd, s).is_some_and(|c| visible_in(s, &c.node_id));
        set_checked(hwnd, ID_VISIBLE, chosen_visible);
        set_text(
            hwnd,
            ID_VISIBLE,
            if s.configs.len() > 1 {
                "&Visible to my other devices in the selected account"
            } else {
                "&Visible to my other devices"
            },
        );
        tray::set_tip(hwnd, &tray_tip(s));
        s.configs
            .iter()
            .map(|c| {
                let running = s.running.iter().any(|(id, _)| id == &c.node_id);
                let state = if !running {
                    "not connected"
                } else if !s.connected.contains(&c.node_id) {
                    "connecting"
                } else if visible_in(s, &c.node_id) {
                    "visible"
                } else {
                    "not visible"
                };
                format!("{}, as \"{}\": {state}", app::account_name(c), c.name)
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    // The accounts list keeps its selection (a screen reader's place), and
    // is rebuilt only when a row changed.
    let changed = with_state(|s| {
        let changed = s.account_rows != rows;
        if changed {
            s.account_rows.clone_from(&rows);
        }
        changed
    })
    .unwrap_or(true);
    if changed {
        let keep = list_selection(hwnd, ID_ACCOUNTS);
        set_list(hwnd, ID_ACCOUNTS, &rows, keep.or(Some(0)));
    }
}

// ─── pages and tabs ─────────────────────────────────────────────────────────

fn show(hwnd: HWND, id: i32, on: bool) {
    // SAFETY: showing or hiding one of our child controls.
    unsafe {
        let _ = ShowWindow(item(hwnd, id), if on { SW_SHOWNA } else { SW_HIDE });
    }
}

/// Shows `page` (and the tab control unless it is the sign-in page),
/// hides every other page's controls, and with `focus` puts focus on the
/// page's first control.
fn show_page(hwnd: HWND, page: Page, focus: bool) {
    let adding = with_state(|s| {
        s.page = page;
        s.adding
    })
    .unwrap_or(false);
    for p in [
        Page::SignIn,
        Page::Devices,
        Page::Streams,
        Page::Accounts,
        Page::Settings,
    ] {
        if p != page {
            for id in p.controls() {
                show(hwnd, *id, false);
            }
        }
    }
    for id in page.controls() {
        show(hwnd, *id, *id != ID_CANCEL_ADD || adding);
    }
    show(hwnd, ID_TABS, page != Page::SignIn);
    if let Some(i) = TABS.iter().position(|t| *t == page) {
        // SAFETY: selecting a tab of our own tab control.
        unsafe {
            let _ = SendMessageW(item(hwnd, ID_TABS), TCM_SETCURSEL, Some(WPARAM(i)), None);
        }
    }
    if focus {
        // SAFETY: focusing one of our child controls.
        unsafe {
            let _ = SetFocus(Some(item(hwnd, page.first())));
        }
    }
}

/// The page the window shows now.
fn current_page() -> Page {
    with_state(|s| s.page).unwrap_or(Page::SignIn)
}

/// Ctrl+Tab (`forward`) or Ctrl+Shift+Tab: the next or previous tab, with
/// focus on its first control. Nothing on the sign-in page.
fn cycle_tab(hwnd: HWND, forward: bool) {
    let page = current_page();
    let Some(i) = TABS.iter().position(|t| *t == page) else {
        return;
    };
    let n = TABS.len();
    let next = TABS[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }];
    show_page(hwnd, next, true);
}

/// The tab chosen on the tab control itself (arrow keys or a click).
fn on_tab_changed(hwnd: HWND) {
    // SAFETY: reading the selection of our own tab control.
    let i = unsafe { SendMessageW(item(hwnd, ID_TABS), TCM_GETCURSEL, None, None) }.0;
    if let Some(page) = usize::try_from(i).ok().and_then(|i| TABS.get(i).copied()) {
        show_page(hwnd, page, false);
    }
}

fn set_tab_text(hwnd: HWND, index: usize, text: &str) {
    let mut wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let tab = TCITEMW {
        mask: TCIF_TEXT,
        pszText: windows::core::PWSTR(wide.as_mut_ptr()),
        ..Default::default()
    };
    // SAFETY: setting a tab's text on our own tab control; `wide` outlives
    // the call.
    unsafe {
        let _ = SendMessageW(
            item(hwnd, ID_TABS),
            TCM_SETITEMW,
            Some(WPARAM(index)),
            Some(LPARAM(&tab as *const TCITEMW as isize)),
        );
    }
}

/// The streams changed: the Streams tab's title holds their count.
pub(crate) fn streams_changed(hwnd: HWND) {
    let n = crate::remote::stream_count();
    let title = if n == 0 {
        "Streams".to_owned()
    } else {
        format!("Streams ({n})")
    };
    let changed = with_state(|s| {
        let changed = s.streams_title != title;
        s.streams_title.clone_from(&title);
        changed
    })
    .unwrap_or(false);
    if changed {
        set_tab_text(hwnd, 1, &title);
    }
}

fn list_selection(hwnd: HWND, id: i32) -> Option<usize> {
    // SAFETY: a standard list-box message to one of our own controls.
    let i = unsafe { SendMessageW(item(hwnd, id), LB_GETCURSEL, None, None) }.0;
    usize::try_from(i).ok()
}

/// Refills a list box; selects `select` (when it exists).
fn set_list(hwnd: HWND, id: i32, rows: &[String], select: Option<usize>) {
    let list = item(hwnd, id);
    // SAFETY: standard list-box messages to our own control; each string
    // outlives its SendMessageW.
    unsafe {
        let _ = SendMessageW(list, LB_RESETCONTENT, None, None);
        for row in rows {
            let t = wide(row);
            let _ = SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        }
        if let Some(i) = select.filter(|i| *i < rows.len()) {
            let _ = SendMessageW(list, LB_SETCURSEL, Some(WPARAM(i)), None);
        }
    }
}

/// The accounts, as the device panel needs them (device id, name).
fn account_views(s: &State) -> Vec<(String, String)> {
    s.configs
        .iter()
        .map(|c| (c.node_id.clone(), app::account_name(c)))
        .collect()
}

/// The tray icon's tooltip and accessible name.
fn tray_tip(s: &State) -> String {
    let visible = s
        .configs
        .iter()
        .filter(|c| visible_in(s, &c.node_id))
        .count();
    match s.configs.as_slice() {
        [] => "AudioNet: not signed in".into(),
        [c] => format!(
            "AudioNet: {}, signed in as {}",
            if visible == 1 {
                "visible"
            } else {
                "not visible"
            },
            c.name
        ),
        more => format!("AudioNet: visible in {visible} of {} accounts", more.len()),
    }
}

fn is_visible(hwnd: HWND) -> bool {
    // SAFETY: querying our own window.
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

/// Brings the window back from the tray (or from minimized) with focus on
/// the page it shows.
fn show_window(hwnd: HWND) {
    // SAFETY: showing, restoring and focusing our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    show_page(hwnd, current_page(), true);
}

/// Opens the window on its Settings tab (the tray menu's Settings…).
fn show_settings(hwnd: HWND) {
    show_window(hwnd);
    if current_page() != Page::SignIn {
        show_page(hwnd, Page::Settings, true);
    }
}

/// Hides the window; AudioNet keeps running in the tray.
fn hide_to_tray(hwnd: HWND) {
    // SAFETY: hiding our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    let first_time =
        with_state(|s| !std::mem::replace(&mut s.tray_notice_shown, true)).unwrap_or(false);
    if first_time {
        tray::notify(
            hwnd,
            "AudioNet is still running",
            "It is in the system tray. To open it, select the AudioNet icon; to exit, choose Exit from its menu.",
            false,
        );
    }
}

/// Connects every account not yet connected: while AudioNet runs, this
/// computer is connected to all of them.
fn start_agent(hwnd: HWND) {
    let configs = with_state(|s| s.configs.clone()).unwrap_or_default();
    for config in configs {
        start_account(hwnd, config);
    }
    refresh(hwnd);
}

/// Whether this computer is visible in an account when it connects: the
/// account's saved choice, else visible (saved, so it stays the account's
/// own choice). The sharing choice of versions before 1.3 is not carried
/// over: every account starts visible.
fn initial_visibility(node_id: &str) -> bool {
    if let Some(on) = settings::account_visible(node_id) {
        return on;
    }
    let _ = settings::set_account_visible(node_id, true);
    true
}

/// Connects one account's agent.
fn start_account(hwnd: HWND, config: NodeConfig) {
    let id = config.node_id.clone();
    if with_state(|s| s.running.iter().any(|(r, _)| r == &id)).unwrap_or(true) {
        return;
    }
    let visible = initial_visibility(&id);
    let h = hwnd.0 as isize;
    let status: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |t: &str| {
        post(
            h,
            WM_APP_STATUS,
            StatusMsg {
                text: t.to_owned(),
                announce: important(t),
            },
        );
    });
    let account = id.clone();
    let events: audionet_node::agent::AppEventFn =
        Arc::new(move |e| post(h, WM_APP_REMOTE, (account.clone(), e)));
    let account = id.clone();
    let running = app::Running::start(config, visible, status, events, move |err| {
        post(h, WM_APP_ENDED, (account, err))
    });
    with_state(|s| s.running.push((id, running)));
}

/// Disconnects every account (leaving AudioNet): this computer is no
/// longer visible anywhere.
fn stop_agent(hwnd: HWND) {
    let running = with_state(|s| {
        s.connected.clear();
        std::mem::take(&mut s.running)
    })
    .unwrap_or_default();
    if !running.is_empty() {
        for (_, mut r) in running {
            r.stop();
        }
        log(
            hwnd,
            "Stopped. This computer is no longer connected to your accounts.",
        );
    }
    crate::remote::on_stopped(hwnd);
    refresh(hwnd);
}

/// Turns this computer's visibility in one account on or off, and
/// remembers the choice.
fn set_visible(hwnd: HWND, config: &NodeConfig, on: bool) {
    let found = with_state(|s| {
        s.running
            .iter()
            .find(|(id, _)| id == &config.node_id)
            .map(|(_, r)| r.set_visible(on))
            .is_some()
    })
    .unwrap_or(false);
    if !found {
        return;
    }
    if let Err(e) = settings::set_account_visible(&config.node_id, on) {
        log(hwnd, &e);
    }
    let name = app::account_name(config);
    let text = if on {
        format!(
            "This computer is visible in {name}: your other devices there see it, and can listen to it and send to it."
        )
    } else {
        format!(
            "This computer is no longer visible in {name}: your other devices there cannot see it or reach it. It can still listen to them and send to them."
        )
    };
    log(hwnd, &text);
    announce(hwnd, &text);
    refresh(hwnd);
}

/// The tray's visibility item: every account.
fn toggle_visible_everywhere(hwnd: HWND) {
    let (configs, any) = with_state(|s| {
        let any = s.configs.iter().any(|c| visible_in(s, &c.node_id));
        (s.configs.clone(), any)
    })
    .unwrap_or_default();
    for c in &configs {
        set_visible(hwnd, c, !any);
    }
}

/// Sends a command to one account's agent; false if it is not running.
fn run_command(account: &str, c: audionet_node::agent::Command) -> bool {
    with_state(|s| {
        s.running
            .iter()
            .find(|(id, _)| id == account)
            .map(|(_, r)| r.command(c))
            .is_some()
    })
    .unwrap_or(false)
}

/// The server address of the first account (or the one typed on the
/// sign-in page), without a trailing slash.
fn server_address(hwnd: HWND) -> String {
    with_state(|s| s.configs.first().map(|c| c.server_url.clone()))
        .flatten()
        .unwrap_or_else(|| get_text(hwnd, ID_SERVER).trim().to_owned())
        .trim_end_matches('/')
        .to_owned()
}

fn on_command(hwnd: HWND, id: i32) {
    match id {
        ID_SIGN_IN => {
            if with_state(|s| s.signing_in).unwrap_or(false) {
                announce(hwnd, "Still signing in.");
                return;
            }
            let server = get_text(hwnd, ID_SERVER).trim().to_owned();
            let user = get_text(hwnd, ID_USER).trim().to_owned();
            let password = get_text(hwnd, ID_PASSWORD);
            let name = get_text(hwnd, ID_NAME).trim().to_owned();
            // The password is not kept: the field is cleared at once.
            set_text(hwnd, ID_PASSWORD, "");
            let missing = [
                (server.is_empty(), ID_SERVER),
                (user.is_empty(), ID_USER),
                (password.is_empty(), ID_PASSWORD),
                (name.is_empty(), ID_NAME),
            ]
            .into_iter()
            .find(|(empty, _)| *empty);
            if let Some((_, focus)) = missing {
                message(
                    hwnd,
                    "Enter the server address, your account name and password, and a name for this computer.",
                    true,
                );
                // SAFETY: focusing one of our child controls.
                unsafe {
                    let _ = SetFocus(Some(item(hwnd, focus)));
                }
                return;
            }
            let already = with_state(|s| {
                s.configs
                    .iter()
                    .find(|c| app::same_account(c, &server, &user))
                    .map(app::account_name)
            })
            .flatten();
            if let Some(name) = already {
                message(
                    hwnd,
                    &format!("This computer is already signed in to {name}."),
                    true,
                );
                return;
            }
            with_state(|s| s.signing_in = true);
            refresh(hwnd);
            log(hwnd, &format!("Signing in to {server} as {user}..."));
            announce(hwnd, "Signing in.");
            let h = hwnd.0 as isize;
            app::sign_in_async(server, user, password, name, move |r| {
                post(h, WM_APP_SIGNED_IN, r)
            });
        }
        ID_CANCEL_ADD => {
            with_state(|s| s.adding = false);
            set_text(hwnd, ID_PASSWORD, "");
            refresh(hwnd);
            show_page(hwnd, Page::Accounts, false);
            // SAFETY: focusing one of our child controls.
            unsafe {
                let _ = SetFocus(Some(item(hwnd, ID_ADD_ACCOUNT)));
            }
        }
        ID_ADD_ACCOUNT => {
            with_state(|s| s.adding = true);
            set_text(hwnd, ID_USER, "");
            set_text(hwnd, ID_PASSWORD, "");
            refresh(hwnd);
            show_page(hwnd, Page::SignIn, true);
            announce(hwnd, "Add an account: sign in to it.");
        }
        ID_VISIBLE => {
            let chosen = with_state(|s| chosen_account(hwnd, s)).flatten();
            if let Some(c) = chosen {
                set_visible(hwnd, &c, is_checked(hwnd, ID_VISIBLE));
            }
        }
        ID_SIGN_OUT => {
            let Some(config) = list_selection(hwnd, ID_ACCOUNTS)
                .and_then(|i| with_state(|s| s.configs.get(i).cloned()).flatten())
            else {
                message(hwnd, "Select an account in Accounts first.", true);
                return;
            };
            let name = app::account_name(&config);
            if confirm(
                hwnd,
                &format!(
                    "Sign out of {name}? This computer is removed from that account. To use it there again, sign in again."
                ),
            ) {
                // Its agent stops first (streams there end), then the
                // server removes this computer's record.
                let running = with_state(|s| {
                    s.connected.retain(|id| id != &config.node_id);
                    let i = s.running.iter().position(|(id, _)| id == &config.node_id)?;
                    Some(s.running.remove(i).1)
                })
                .flatten();
                if let Some(mut r) = running {
                    r.stop();
                }
                crate::remote::on_account_stopped(hwnd, &config.node_id);
                log(hwnd, &format!("Signing out of {name}..."));
                announce(hwnd, "Signing out.");
                let h = hwnd.0 as isize;
                let c = config.clone();
                app::remove_async(config, move |r| post(h, WM_APP_SIGNED_OUT, (c, r)));
                refresh(hwnd);
            }
        }
        ID_MANAGE => {
            // Renaming and removing devices is in the web client.
            if open_in_browser(hwnd, &format!("{}/?manage", server_address(hwnd))) {
                log(
                    hwnd,
                    "Opened Manage devices in the web browser: rename or remove your devices there.",
                );
            }
        }
        ID_OPEN_WEB => {
            open_in_browser(hwnd, &server_address(hwnd));
        }
        ID_FORGOT => {
            // Passwords are reset in the web client of the server typed
            // above, which emails a link to the account's address.
            let server = get_text(hwnd, ID_SERVER)
                .trim()
                .trim_end_matches('/')
                .to_owned();
            if open_in_browser(hwnd, &format!("{server}/?forgot")) {
                log(
                    hwnd,
                    "Opened password reset in the web browser: enter your account name or email address there.",
                );
            } else {
                message(
                    hwnd,
                    "Enter the server address first, starting with https://.",
                    true,
                );
            }
        }
        ID_LOG => crate::logwin::open(hwnd),
        ID_EXIT => exit_app(hwnd),
        _ => {}
    }
}

/// Opens an http(s) URL in the default browser; false for anything else.
fn open_in_browser(hwnd: HWND, url: &str) -> bool {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return false;
    }
    // SAFETY: opening a URL with the user's default browser.
    unsafe {
        ShellExecuteW(
            Some(hwnd),
            w!("open"),
            &wide(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
    true
}

pub(crate) fn is_checked(hwnd: HWND, id: i32) -> bool {
    // SAFETY: querying a checkbox we created.
    unsafe { SendMessageW(item(hwnd, id), BM_GETCHECK, None, None).0 == 1 }
}

pub(crate) fn set_checked(hwnd: HWND, id: i32, on: bool) {
    // SAFETY: setting a checkbox we created.
    unsafe {
        let _ = SendMessageW(
            item(hwnd, id),
            BM_SETCHECK,
            Some(WPARAM(usize::from(on))),
            None,
        );
    }
}

pub(crate) fn message(hwnd: HWND, text: &str, error: bool) {
    let style = if error {
        MB_ICONWARNING
    } else {
        MB_ICONINFORMATION
    };
    // SAFETY: modal message box owned by our window.
    unsafe {
        MessageBoxW(Some(hwnd), &wide(text), w!("AudioNet"), style | MB_OK);
    }
}

fn confirm(hwnd: HWND, text: &str) -> bool {
    // SAFETY: modal message box owned by our window.
    unsafe {
        MessageBoxW(
            Some(hwnd),
            &wide(text),
            w!("AudioNet"),
            MB_ICONQUESTION | MB_YESNO,
        ) == IDYES
    }
}

pub(crate) fn copy_to_clipboard(hwnd: HWND, text: &str) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    let data: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: standard clipboard sequence; the global memory block is sized
    // for `data`, filled while locked, and owned by the clipboard afterwards.
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2) {
            let p = GlobalLock(mem) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
                let _ = GlobalUnlock(mem);
                let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// The window's close button or Alt+F4: to the tray, or exit.
fn close_window(hwnd: HWND) {
    if settings::get(settings::CLOSE_TO_TRAY) {
        hide_to_tray(hwnd);
    } else {
        exit_app(hwnd);
    }
}

/// Starts an update check on a background thread (download and unpack
/// included). `manual`: the user asked, so report "up to date" too.
pub(crate) fn check_for_updates(hwnd: HWND, manual: bool) {
    let Some((url, key)) = update::configured() else {
        if manual {
            message(
                hwnd,
                "This copy of AudioNet does not update itself: it was built without an update source.",
                false,
            );
        }
        return;
    };
    let busy = with_state(|s| {
        let busy = s.updates.checking || s.updates.prepared.is_some();
        if !busy {
            s.updates.checking = true;
            s.updates.next_check = std::time::Instant::now() + CHECK_EVERY;
        }
        busy
    })
    .unwrap_or(true);
    if busy {
        if manual {
            try_install_update(hwnd, true);
        }
        return;
    }
    if manual {
        update_note(hwnd, "Checking for updates.", true);
    }
    // A release that was installed but still reports an older version is
    // not offered again (it would loop).
    let skip = settings::get_string(settings::UPDATED_TO)
        .filter(|v| update::is_newer(v, update::current_version()));
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let result = (|| {
            let Some(m) = update::check(url, key, skip.as_deref())? else {
                return Ok(None);
            };
            post(
                h,
                WM_APP_STATUS,
                StatusMsg {
                    text: format!("Downloading AudioNet {}.", m.version),
                    announce: false,
                },
            );
            let dir = install_dir()?;
            update::prepare(url, &m, &std::env::temp_dir(), &dir).map(Some)
        })();
        post(h, WM_APP_UPDATE, UpdateOutcome { manual, result });
    });
}

fn install_dir() -> Result<std::path::PathBuf, String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .ok_or_else(|| "could not find where AudioNet is installed".into())
}

fn on_update_outcome(hwnd: HWND, outcome: UpdateOutcome) {
    with_state(|s| s.updates.checking = false);
    match outcome.result {
        Ok(None) => {
            if outcome.manual {
                let text = format!(
                    "AudioNet is up to date (version {}).",
                    update::current_version()
                );
                update_note(hwnd, &text, true);
            }
        }
        Ok(Some(prepared)) => {
            update_note(
                hwnd,
                &format!("AudioNet {} is downloaded and checked.", prepared.version),
                false,
            );
            with_state(|s| {
                s.updates.prepared = Some(prepared);
                s.updates.told_waiting = false;
            });
            try_install_update(hwnd, outcome.manual);
        }
        Err(e) => {
            with_state(|s| s.updates.next_check = std::time::Instant::now() + RETRY_AFTER_FAILURE);
            let text = format!("Could not update AudioNet: {e}");
            update_note(hwnd, &text, outcome.manual);
        }
    }
}

/// An update result: in the status log, in Settings (if open) and, when
/// `speak`, announced.
fn update_note(hwnd: HWND, text: &str, speak: bool) {
    log(hwnd, text);
    crate::prefs::set_update_status(text);
    if speak {
        announce(hwnd, text);
    }
}

/// Installs a prepared update unless someone is using this computer's
/// audio right now, in which case it waits (the timer tries again).
fn try_install_update(hwnd: HWND, manual: bool) {
    let busy = with_state(|s| {
        s.running
            .iter()
            .map(|(_, r)| r.active_sessions())
            .sum::<usize>()
    })
    .unwrap_or(0)
        > 0;
    if busy {
        let tell = with_state(|s| {
            let first = !s.updates.told_waiting && s.updates.prepared.is_some();
            s.updates.told_waiting = true;
            first
        })
        .unwrap_or(false);
        if tell || manual {
            let text = "The update will be installed when no one is listening or talking through this computer.";
            update_note(hwnd, text, manual);
        }
        return;
    }
    let Some(prepared) = with_state(|s| s.updates.prepared.take()).flatten() else {
        return;
    };
    let dir = match install_dir() {
        Ok(d) => d,
        Err(e) => {
            log(hwnd, &format!("Could not update AudioNet: {e}"));
            return;
        }
    };
    log(hwnd, &format!("Installing AudioNet {}.", prepared.version));
    let installed = match update::install(&prepared, &dir) {
        Ok(i) => i,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&prepared.staging);
            log(hwnd, &format!("Could not update AudioNet: {e}"));
            return;
        }
    };
    // Start the new copy and wait for it to report that it runs.
    let Some(event) = handoff::started_event() else {
        update::rollback(installed);
        log(
            hwnd,
            "Could not update AudioNet: Windows refused to create the hand-over signal.",
        );
        return;
    };
    let mut command = std::process::Command::new(dir.join("audionet-desktop.exe"));
    command.args([
        "--after-update",
        &std::process::id().to_string(),
        "--from",
        update::current_version(),
    ]);
    if is_visible(hwnd) {
        command.arg("--show");
    }
    let started = command
        .spawn()
        .map(|mut child| {
            if handoff::wait_started(&event, HANDOFF_TIMEOUT) {
                true
            } else {
                let _ = child.kill();
                false
            }
        })
        .unwrap_or(false);
    if !started {
        update::rollback(installed);
        log(
            hwnd,
            &format!(
                "The new version did not start, so AudioNet stays at version {}.",
                update::current_version()
            ),
        );
        return;
    }
    let _ = settings::set_string(settings::UPDATED_TO, &prepared.version);
    // The new copy takes over: leave without asking.
    stop_agent(hwnd);
    // SAFETY: destroying our own top-level window ends this copy.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// Runs the update schedule (from the timer).
fn update_tick(hwnd: HWND) {
    if update::configured().is_none() || !settings::get(settings::AUTO_UPDATE) {
        return;
    }
    let (due, has_prepared) = with_state(|s| {
        (
            !s.updates.checking && std::time::Instant::now() >= s.updates.next_check,
            s.updates.prepared.is_some(),
        )
    })
    .unwrap_or((false, false));
    if has_prepared {
        try_install_update(hwnd, false);
    } else if due {
        check_for_updates(hwnd, false);
    }
}

/// Really quits (after confirming if this computer is visible somewhere).
fn exit_app(hwnd: HWND) {
    let visible =
        with_state(|s| s.configs.iter().any(|c| visible_in(s, &c.node_id))).unwrap_or(false);
    if visible
        && !confirm(
            hwnd,
            "Exit AudioNet? This computer will no longer be visible to your other devices, and its streams end.",
        )
    {
        return;
    }
    stop_agent(hwnd);
    // SAFETY: destroying our own top-level window.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// A standard control to create with `add` (used by the remote panel).
pub(crate) struct Ctl {
    class: PCWSTR,
    text: String,
    style: WINDOW_STYLE,
    ex: WINDOW_EX_STYLE,
    id: i32,
}

impl Ctl {
    pub(crate) fn label(text: &str, id: i32) -> Self {
        Self {
            class: w!("STATIC"),
            text: text.to_owned(),
            style: WINDOW_STYLE(0),
            ex: WINDOW_EX_STYLE(0),
            id,
        }
    }

    pub(crate) fn button(text: &str, id: i32) -> Self {
        Self {
            class: w!("BUTTON"),
            text: text.to_owned(),
            style: WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
            ex: WINDOW_EX_STYLE(0),
            id,
        }
    }

    /// A drop-down list (choose one; no typing).
    pub(crate) fn combo(id: i32) -> Self {
        Self {
            class: w!("COMBOBOX"),
            text: String::new(),
            style: WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_VSCROLL | WS_TABSTOP,
            ex: WINDOW_EX_STYLE(0),
            id,
        }
    }

    /// Any other standard control class.
    pub(crate) fn custom(class: PCWSTR, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: i32) -> Self {
        Self {
            class,
            text: String::new(),
            style,
            ex,
            id,
        }
    }

    /// A single-selection list box that reports selection changes.
    pub(crate) fn checkbox(text: &str, id: i32) -> Self {
        Self {
            class: w!("BUTTON"),
            text: text.to_owned(),
            style: WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
            ex: WINDOW_EX_STYLE(0),
            id,
        }
    }

    pub(crate) fn listbox(id: i32) -> Self {
        Self {
            class: w!("LISTBOX"),
            text: String::new(),
            style: WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32)
                | WS_VSCROLL
                | WS_TABSTOP,
            ex: WS_EX_CLIENTEDGE,
            id,
        }
    }
}

/// Creates a `Ctl` at DPI-scaled coordinates.
pub(crate) fn add(parent: HWND, c: Ctl, rect: (i32, i32, i32, i32), scale: f32, font: HFONT) {
    control(
        parent, c.class, &c.text, c.style, c.ex, c.id, rect, scale, font,
    );
}

/// Creates a child control at DPI-scaled coordinates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn control(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    ex: WINDOW_EX_STYLE,
    id: i32,
    rect: (i32, i32, i32, i32),
    scale: f32,
    font: HFONT,
) {
    let (x, y, w, h) = rect;
    let s = |v: i32| (v as f32 * scale) as i32;
    // SAFETY: creating a standard control as a child of our window; the
    // text outlives the call; the font handle lives as long as the window.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        if let Ok(h) = CreateWindowExW(
            ex,
            class,
            &wide(text),
            style | WS_CHILD | WS_VISIBLE,
            s(x),
            s(y),
            s(w),
            s(h),
            Some(parent),
            Some(HMENU(id as isize as *mut _)),
            Some(hinst.into()),
            None,
        ) {
            let _ = SendMessageW(
                h,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }
}

pub(crate) fn message_font(scale: f32) -> HFONT {
    // SAFETY: SystemParametersInfoW fills a correctly sized NONCLIENTMETRICSW.
    unsafe {
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        if SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some((&mut ncm as *mut NONCLIENTMETRICSW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        {
            let mut lf = ncm.lfMessageFont;
            lf.lfHeight = (lf.lfHeight as f32 * scale) as i32;
            return CreateFontIndirectW(&lf);
        }
        HFONT(GetStockObject(windows::Win32::Graphics::Gdi::DEFAULT_GUI_FONT).0)
    }
}

/// The window's client size (before DPI scaling).
const WINDOW_W: i32 = 600;
const WINDOW_H: i32 = 640;
/// Where a tab's controls start, and how wide they are.
const PAGE_X: i32 = 24;
const PAGE_Y: i32 = 48;
const PAGE_W: i32 = WINDOW_W - 2 * PAGE_X;

fn build(hwnd: HWND) {
    // SAFETY: querying our own window's DPI.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let scale = dpi as f32 / 96.0;
    let font = message_font(scale);
    let st = WINDOW_STYLE(0);
    let tab = WS_TABSTOP;
    let edit_style = WINDOW_STYLE(ES_AUTOHSCROLL as u32) | tab;
    let border = WS_EX_CLIENTEDGE;
    let button = WINDOW_STYLE(BS_PUSHBUTTON as u32) | tab;
    let none = WINDOW_EX_STYLE(0);
    let x = 16;
    let full = WINDOW_W - 2 * x;
    // SAFETY: registering the common-controls class the tab control needs.
    unsafe {
        let icc = windows::Win32::UI::Controls::INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<windows::Win32::UI::Controls::INITCOMMONCONTROLSEX>()
                as u32,
            dwICC: windows::Win32::UI::Controls::ICC_TAB_CLASSES,
        };
        let _ = windows::Win32::UI::Controls::InitCommonControlsEx(&icc);
    }

    // The tab control first: first in Tab order. WS_CLIPSIBLINGS keeps it
    // from painting over the pages' controls, which are its siblings.
    // Its window text is its accessible name ("Pages, tab control").
    control(
        hwnd,
        windows::Win32::UI::Controls::WC_TABCONTROLW,
        "Pages",
        tab | WS_CLIPSIBLINGS,
        none,
        ID_TABS,
        (8, 8, WINDOW_W - 16, WINDOW_H - 60),
        scale,
        font,
    );
    for (i, title) in ["Devices", "Streams", "Accounts", "Settings"]
        .iter()
        .enumerate()
    {
        let mut wide: Vec<u16> = title.encode_utf16().chain([0]).collect();
        let t = TCITEMW {
            mask: TCIF_TEXT,
            pszText: windows::core::PWSTR(wide.as_mut_ptr()),
            ..Default::default()
        };
        // SAFETY: adding a tab to our own tab control; `wide` outlives the
        // call.
        unsafe {
            let _ = SendMessageW(
                item(hwnd, ID_TABS),
                TCM_INSERTITEMW,
                Some(WPARAM(i)),
                Some(LPARAM(&t as *const TCITEMW as isize)),
            );
        }
    }

    // ── The sign-in page (no tabs) ──
    control(
        hwnd,
        w!("STATIC"),
        "Sign in to your AudioNet account to add this computer to it.",
        st,
        none,
        ID_SIGN_IN_HEADING,
        (x, 14, full, 24),
        scale,
        font,
    );
    control(
        hwnd,
        w!("STATIC"),
        "&Server address:",
        st,
        none,
        ID_SERVER - 1,
        (x, 46, full, 20),
        scale,
        font,
    );
    control(
        hwnd,
        w!("EDIT"),
        app::default_server(),
        edit_style,
        border,
        ID_SERVER,
        (x, 68, full, 26),
        scale,
        font,
    );
    let half = (full - 16) / 2;
    control(
        hwnd,
        w!("STATIC"),
        "Account &user name:",
        st,
        none,
        ID_USER - 1,
        (x, 104, half, 20),
        scale,
        font,
    );
    control(
        hwnd,
        w!("EDIT"),
        "",
        edit_style,
        border,
        ID_USER,
        (x, 126, half, 26),
        scale,
        font,
    );
    control(
        hwnd,
        w!("STATIC"),
        "Pass&word:",
        st,
        none,
        ID_PASSWORD - 1,
        (x + half + 16, 104, half, 20),
        scale,
        font,
    );
    control(
        hwnd,
        w!("EDIT"),
        "",
        edit_style | WINDOW_STYLE(ES_PASSWORD as u32),
        border,
        ID_PASSWORD,
        (x + half + 16, 126, half, 26),
        scale,
        font,
    );
    control(
        hwnd,
        w!("STATIC"),
        "&Name for this computer:",
        st,
        none,
        ID_NAME - 1,
        (x, 162, full, 20),
        scale,
        font,
    );
    control(
        hwnd,
        w!("EDIT"),
        &app::computer_name(),
        edit_style,
        border,
        ID_NAME,
        (x, 184, 300, 26),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "Sign &in",
        WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | tab,
        none,
        ID_SIGN_IN,
        (x, 224, 140, 30),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "&Forgot password…",
        button,
        none,
        ID_FORGOT,
        (x + 150, 224, 180, 30),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "&Cancel",
        button,
        none,
        ID_CANCEL_ADD,
        (x + 340, 224, 110, 30),
        scale,
        font,
    );

    // ── Devices and Streams ──
    crate::remote::build(hwnd, PAGE_X, PAGE_Y, PAGE_W, scale, font);

    // ── Accounts ──
    let (px, py, pw) = (PAGE_X, PAGE_Y, PAGE_W);
    control(
        hwnd,
        w!("STATIC"),
        "&Accounts this computer is signed in to:",
        st,
        none,
        ID_ACCOUNTS - 1,
        (px, py, pw, 20),
        scale,
        font,
    );
    control(
        hwnd,
        w!("LISTBOX"),
        "",
        WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32) | WS_VSCROLL | tab,
        border,
        ID_ACCOUNTS,
        (px, py + 22, pw, 100),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "&Visible to my other devices",
        WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | tab,
        none,
        ID_VISIBLE,
        (px, py + 132, pw, 26),
        scale,
        font,
    );
    control(
        hwnd,
        w!("STATIC"),
        "Visible: your other devices see this computer, and can listen to it and send to it. Not visible: it is hidden from them and they cannot reach it, but it can still listen to them and send to them.",
        st,
        none,
        ID_VISIBLE_HELP,
        (px, py + 162, pw, 60),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "Sign &out of the selected account",
        button,
        none,
        ID_SIGN_OUT,
        (px, py + 232, 270, 30),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "A&dd account…",
        button,
        none,
        ID_ADD_ACCOUNT,
        (px + 280, py + 232, 160, 30),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "&Manage devices…",
        button,
        none,
        ID_MANAGE,
        (px, py + 272, 180, 30),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "Open &web client",
        button,
        none,
        ID_OPEN_WEB,
        (px + 190, py + 272, 170, 30),
        scale,
        font,
    );

    // ── Settings ──
    crate::prefs::build(hwnd, PAGE_X, PAGE_Y, PAGE_W, scale, font);

    // ── Always ──
    control(
        hwnd,
        w!("BUTTON"),
        "Status &log…",
        button,
        none,
        ID_LOG,
        (x, WINDOW_H - 42, 140, 30),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "E&xit",
        button,
        none,
        ID_EXIT,
        (WINDOW_W - x - 110, WINDOW_H - 42, 110, 30),
        scale,
        font,
    );
    with_state(|s| s.font = font);
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg != 0 && msg == TASKBAR_CREATED.load(Relaxed) {
        let tip = with_state(|s| tray_tip(s)).unwrap_or_default();
        tray::add(hwnd, &tip);
        return LRESULT(0);
    }
    if msg != 0 && msg == SHOW_REQUEST.load(Relaxed) {
        show_window(hwnd);
        return LRESULT(0);
    }
    match msg {
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF == WA_INACTIVE as usize {
                // SAFETY: reading the focus and checking it is one of ours.
                unsafe {
                    let f = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus();
                    if !f.is_invalid() && IsChild(hwnd, f).as_bool() {
                        LAST_FOCUS.store(f.0 as isize, Relaxed);
                    }
                }
            }
            // SAFETY: default handling (it gives the window focus when it is
            // activated, which WM_SETFOCUS below passes on).
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SETFOCUS => {
            // The window itself never keeps focus: back to the control that
            // had it, or the page's first control.
            let saved = HWND(LAST_FOCUS.load(Relaxed) as *mut _);
            // SAFETY: checking and focusing our own child controls.
            unsafe {
                let usable = !saved.is_invalid()
                    && IsChild(hwnd, saved).as_bool()
                    && IsWindowVisible(saved).as_bool()
                    && windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(saved)
                        .as_bool();
                let target = if usable {
                    saved
                } else {
                    let page = STATE
                        .with(|s| s.try_borrow().ok().and_then(|s| s.as_ref().map(|s| s.page)))
                        .unwrap_or(Page::SignIn);
                    item(hwnd, page.first())
                };
                let _ = SetFocus(Some(target));
            }
            LRESULT(0)
        }
        tray::WM_APP_TRAY => {
            match tray::event(wparam.0, lparam.0) {
                Some(TrayEvent::Open) => show_window(hwnd),
                Some(TrayEvent::Menu(x, y)) => {
                    let (visible, signed_in) = with_state(|s| {
                        (
                            s.configs.iter().any(|c| visible_in(s, &c.node_id)),
                            !s.configs.is_empty(),
                        )
                    })
                    .unwrap_or((false, false));
                    let (chosen, previous) = tray::menu(hwnd, x, y, visible, signed_in);
                    match chosen {
                        Some(tray::MENU_OPEN) => show_window(hwnd),
                        Some(tray::MENU_TOGGLE) => toggle_visible_everywhere(hwnd),
                        Some(tray::MENU_SETTINGS) => show_settings(hwnd),
                        Some(tray::MENU_EXIT) => exit_app(hwnd),
                        _ => {}
                    }
                    // Showing the menu made our (hidden) window the active
                    // one; unless the choice opened it, hand focus back to
                    // where the user was (normally the tray icon), so
                    // screen readers do not read the hidden window.
                    if !is_visible(hwnd) {
                        tray::restore_focus(hwnd, previous);
                    }
                }
                None => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: for WM_NOTIFY, lParam points to an NMHDR from one of
            // our child controls, valid for this call.
            let header = unsafe { &*(lparam.0 as *const windows::Win32::UI::Controls::NMHDR) };
            if header.idFrom == ID_TABS as usize
                && header.code == windows::Win32::UI::Controls::TCN_SELCHANGE
            {
                on_tab_changed(hwnd);
                return LRESULT(0);
            }
            let handled = crate::remote::on_notify(hwnd, header, &run_command);
            if handled {
                return LRESULT(1);
            }
            // SAFETY: default handling for other notifications.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_HSCROLL => {
            // The stream volume trackbar.
            let from = HWND(lparam.0 as *mut _);
            crate::remote::on_scroll(hwnd, from, &run_command);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = (wparam.0 >> 16) as u32;
            if crate::remote::owns(id) {
                crate::remote::on_command(hwnd, id, code, &run_command);
            } else if crate::prefs::owns(id) {
                if code == BN_CLICKED {
                    crate::prefs::on_command(hwnd, id);
                }
            } else if id == IDOK.0 {
                // Enter: the sign-in page's default button.
                if current_page() == Page::SignIn {
                    on_command(hwnd, ID_SIGN_IN);
                }
            } else if id == ID_ACCOUNTS && code == LBN_SELCHANGE {
                // The Visible checkbox acts on the selected account.
                refresh(hwnd);
            } else if code == BN_CLICKED {
                on_command(hwnd, id);
            }
            LRESULT(0)
        }
        DM_GETDEFID => {
            // Enter on the sign-in page signs in; the tabs have no default
            // button (Enter belongs to the focused control).
            if current_page() == Page::SignIn {
                LRESULT(((DC_HASDEFID as isize) << 16) | ID_SIGN_IN as isize)
            } else {
                LRESULT(0)
            }
        }
        WM_APP_STATUS => {
            // SAFETY: posted by `post` with a Box<StatusMsg>.
            let m = unsafe { Box::from_raw(lparam.0 as *mut StatusMsg) };
            log(hwnd, &m.text);
            if m.announce {
                announce(hwnd, &m.text);
            }
            LRESULT(0)
        }
        WM_APP_SIGNED_IN => {
            // SAFETY: posted by `post` with a Box<Result<NodeConfig, String>>.
            let r = unsafe { Box::from_raw(lparam.0 as *mut Result<NodeConfig, String>) };
            with_state(|s| s.signing_in = false);
            match *r {
                Ok(c) => {
                    let text = format!(
                        "Signed in as \"{}\" to {}. This computer is visible there: your other devices see it, and can listen to it and send to it. To hide it, turn off Visible in Accounts.",
                        c.name,
                        app::account_name(&c)
                    );
                    let (accounts, index, adding) = with_state(|s| {
                        s.configs.push(c.clone());
                        let adding = std::mem::replace(&mut s.adding, false);
                        (account_views(s), s.configs.len() - 1, adding)
                    })
                    .unwrap_or_default();
                    crate::remote::set_accounts(hwnd, accounts);
                    start_account(hwnd, c);
                    // The new account is the one the Visible checkbox acts on.
                    refresh(hwnd);
                    // SAFETY: selecting a row of our own list box.
                    unsafe {
                        let _ = SendMessageW(
                            item(hwnd, ID_ACCOUNTS),
                            LB_SETCURSEL,
                            Some(WPARAM(index)),
                            None,
                        );
                    }
                    log(hwnd, &text);
                    announce(hwnd, &text);
                    refresh(hwnd);
                    // The first account opens Devices; another one goes back
                    // to Accounts, where it was added.
                    show_page(
                        hwnd,
                        if adding {
                            Page::Accounts
                        } else {
                            Page::Devices
                        },
                        true,
                    );
                }
                Err(e) => {
                    refresh(hwnd);
                    let hint = if e.contains("password is incorrect") {
                        " If you forgot the password, press Forgot password."
                    } else {
                        ""
                    };
                    log(hwnd, &format!("Signing in failed: {e}{hint}"));
                    message(hwnd, &format!("Signing in failed: {e}{hint}"), true);
                }
            }
            LRESULT(0)
        }
        WM_APP_SIGNED_OUT => {
            // SAFETY: posted by `post` with a Box<(NodeConfig, Result<(), String>)>.
            let done = unsafe { Box::from_raw(lparam.0 as *mut (NodeConfig, Result<(), String>)) };
            let (config, result) = *done;
            let name = app::account_name(&config);
            match result.and_then(|()| app::forget_config(&config.node_id)) {
                Ok(()) => {
                    settings::forget_account_visible(&config.node_id);
                    let accounts = with_state(|s| {
                        s.configs.retain(|c| c.node_id != config.node_id);
                        account_views(s)
                    })
                    .unwrap_or_default();
                    crate::remote::set_accounts(hwnd, accounts);
                    let text = format!(
                        "Signed out of {name}: this computer was removed from that account."
                    );
                    log(hwnd, &text);
                    announce(hwnd, &text);
                    refresh(hwnd);
                    let none_left = with_state(|s| s.configs.is_empty()).unwrap_or(true);
                    if none_left {
                        with_state(|s| s.adding = false);
                        refresh(hwnd);
                        show_page(hwnd, Page::SignIn, true);
                    } else {
                        // SAFETY: focusing one of our child controls.
                        unsafe {
                            let _ = SetFocus(Some(item(hwnd, ID_ACCOUNTS)));
                        }
                    }
                }
                Err(e) => {
                    // Still signed in there: back online as before.
                    start_account(hwnd, config);
                    refresh(hwnd);
                    let text = format!(
                        "Could not sign out of {name}: {e} This computer is still signed in there."
                    );
                    log(hwnd, &text);
                    message(hwnd, &text, true);
                }
            }
            LRESULT(0)
        }
        WM_APP_ENDED => {
            // SAFETY: posted by `post` with a Box<(String, Option<String>)>.
            let ended = unsafe { Box::from_raw(lparam.0 as *mut (String, Option<String>)) };
            let (account, err) = *ended;
            if let Some(e) = err {
                with_state(|s| s.running.retain(|(id, _)| id != &account));
                crate::remote::on_account_stopped(hwnd, &account);
                log(hwnd, &format!("Stopped: {e}"));
                if is_visible(hwnd) {
                    announce(hwnd, &format!("AudioNet stopped: {e}"));
                } else {
                    tray::notify(hwnd, "AudioNet stopped", &e, true);
                }
                refresh(hwnd);
            }
            LRESULT(0)
        }
        WM_APP_REMOTE => {
            // SAFETY: posted by `post` with a Box<(String, AppEvent)>.
            let event =
                unsafe { Box::from_raw(lparam.0 as *mut (String, audionet_node::agent::AppEvent)) };
            let (account, event) = *event;
            let connection = match &event {
                audionet_node::agent::AppEvent::Connected { .. } => Some(true),
                audionet_node::agent::AppEvent::Disconnected { .. } => Some(false),
                _ => None,
            };
            if let Some(up) = connection {
                with_state(|s| {
                    s.connected.retain(|id| id != &account);
                    if up {
                        s.connected.push(account.clone());
                    }
                });
                refresh(hwnd);
            }
            crate::remote::on_event(hwnd, &account, event);
            LRESULT(0)
        }
        WM_APP_UPDATE => {
            // SAFETY: posted by `post` with a Box<UpdateOutcome>.
            let outcome = unsafe { Box::from_raw(lparam.0 as *mut UpdateOutcome) };
            on_update_outcome(hwnd, *outcome);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_UPDATE => {
            update_tick(hwnd);
            LRESULT(0)
        }
        WM_APP_DEVICES => {
            // SAFETY: a one-shot timer owned by our window; setting it again
            // restarts it, so a burst of changes is read once.
            unsafe {
                SetTimer(Some(hwnd), TIMER_DEVICES, DEVICES_SETTLE_MS, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_DEVICES => {
            // SAFETY: the timer set for WM_APP_DEVICES, on our window.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_DEVICES);
            }
            crate::remote::local_devices_changed(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            close_window(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            tray::remove(hwnd);
            if let Some(font) = with_state(|s| s.font) {
                // SAFETY: the font was created in `build` and is no longer used.
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(font.0));
                }
            }
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub fn run(launch: Launch) {
    let background = launch.background;
    // SAFETY: registering process-independent message ids by name.
    let (taskbar_created, show_request) = unsafe {
        (
            RegisterWindowMessageW(w!("TaskbarCreated")),
            RegisterWindowMessageW(w!("AudioNetDesktop.ShowWindow")),
        )
    };
    TASKBAR_CREATED.store(taskbar_created, Relaxed);
    SHOW_REQUEST.store(show_request, Relaxed);
    // Only one copy runs: a second start shows the existing window (unless
    // it is the sign-in start, which has nothing to show).
    // SAFETY: looking up a top-level window by our class name and posting
    // it a registered message.
    unsafe {
        if let Ok(existing) = FindWindowW(crate::profile::class_name(), PCWSTR::null())
            && !existing.is_invalid()
        {
            if !background {
                let _ = PostMessageW(Some(existing), show_request, WPARAM(0), LPARAM(0));
            }
            return;
        }
    }
    // SAFETY: process-wide DPI awareness, set once before creating windows.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // SAFETY: standard window-class registration and window creation.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = crate::profile::class_name();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(GetStockObject(WHITE_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: WINDOW_W,
            bottom: WINDOW_H,
        };
        let _ = AdjustWindowRectEx(&mut rect, style, false, WS_EX_CONTROLPARENT);
        match CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class,
            w!("AudioNet"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return,
        }
    };
    // Resize for the monitor's DPI now that the window exists.
    // SAFETY: querying and resizing our own window.
    unsafe {
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: (WINDOW_W as f32 * scale) as i32,
            bottom: (WINDOW_H as f32 * scale) as i32,
        };
        let _ = AdjustWindowRectEx(&mut rect, style, false, WS_EX_CONTROLPARENT);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            // NOACTIVATE: when starting in the tray the window stays hidden
            // and must not take focus from the program the user is in.
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }

    let configs = app::load_configs();
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd,
            font: HFONT::default(),
            configs,
            running: Vec::new(),
            connected: Vec::new(),
            account_rows: Vec::new(),
            signing_in: false,
            page: Page::SignIn,
            adding: false,
            streams_title: "Streams".into(),
            tray_notice_shown: false,
            updates: Updates {
                next_check: std::time::Instant::now() + FIRST_CHECK,
                checking: false,
                prepared: None,
                told_waiting: false,
            },
        });
    });
    build(hwnd);
    let signed_in = with_state(|s| s.configs.first().cloned()).flatten();
    let accounts = with_state(|s| account_views(s)).unwrap_or_default();
    crate::remote::set_accounts(hwnd, accounts);
    if let Some(c) = &signed_in {
        set_text(hwnd, ID_SERVER, &c.server_url);
        set_text(hwnd, ID_NAME, &c.name);
        log(
            hwnd,
            "Welcome back. This computer connects to your accounts; in each one it is visible to your other devices or not, as chosen in Accounts.",
        );
    } else {
        log(
            hwnd,
            "To add this computer, sign in with your AudioNet account name and password.",
        );
    }
    // "Show measurements" (Settings), before any account connects.
    crate::app::MEASUREMENTS.store(
        settings::get(settings::SHOW_MEASUREMENTS),
        std::sync::atomic::Ordering::Relaxed,
    );
    let tip = with_state(|s| tray_tip(s)).unwrap_or_default();
    tray::add(hwnd, &tip);
    refresh(hwnd);
    let first_page = if signed_in.is_some() {
        Page::Devices
    } else {
        Page::SignIn
    };
    show_page(hwnd, first_page, false);

    // After an update: come back the way the previous copy was, and remove
    // its renamed files. Otherwise: in the tray at sign-in, or every time
    // if the user chose to.
    if let Ok(dir) = install_dir() {
        update::clean_up(&dir);
    }
    let in_tray = match &launch.after_update {
        Some(u) => !u.show,
        None => background || settings::get(settings::START_IN_TRAY),
    };
    if !in_tray {
        // SAFETY: showing our window and focusing a first control.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = UpdateWindow(hwnd);
            let _ = SetFocus(Some(item(hwnd, first_page.first())));
        }
    }
    // Connected to every account while AudioNet runs; visible in each or
    // not as chosen there (new accounts start visible).
    if signed_in.is_some() {
        start_agent(hwnd);
    }
    if let Some(u) = &launch.after_update {
        let text = format!(
            "AudioNet was updated from version {} to {}.",
            u.from,
            update::current_version()
        );
        log(hwnd, &text);
        if in_tray {
            tray::notify(hwnd, "AudioNet updated", &text, false);
        } else {
            announce(hwnd, &text);
        }
    }
    // SAFETY: a periodic timer owned by our window.
    unsafe {
        SetTimer(Some(hwnd), TIMER_UPDATE, TIMER_PERIOD_MS, None);
    }
    // "Play it on" and "Send from this computer" follow device changes
    // (kept until the message loop ends).
    let window = hwnd.0 as isize;
    let _device_watch = audionet_wasapi::notify::watch_devices(move || {
        // SAFETY: posting a message with no payload to our window; if the
        // window is gone the post fails harmlessly.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(window as *mut _)),
                WM_APP_DEVICES,
                WPARAM(0),
                LPARAM(0),
            );
        }
    })
    .map_err(|e| log(hwnd, &format!("Audio devices will not be followed: {e}")))
    .ok();

    // SAFETY: standard message loop; IsDialogMessageW supplies keyboard
    // navigation (Tab, arrows, access keys, Enter) for our controls.
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // The status log (when open) and the main window each get
            // keyboard navigation for their own controls.
            let owned = crate::logwin::handle()
                .filter(|p| msg.hwnd == *p || IsChild(*p, msg.hwnd).as_bool());
            // Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+Page Down and Ctrl+Page Up move
            // between tabs from anywhere in the main window.
            if owned.is_none()
                && msg.message == WM_KEYDOWN
                && (msg.hwnd == hwnd || IsChild(hwnd, msg.hwnd).as_bool())
            {
                use windows::Win32::UI::Input::KeyboardAndMouse::{
                    GetKeyState, VK_CONTROL, VK_NEXT, VK_PRIOR, VK_SHIFT, VK_TAB,
                };
                let ctrl = GetKeyState(i32::from(VK_CONTROL.0)) < 0;
                let shift = GetKeyState(i32::from(VK_SHIFT.0)) < 0;
                let key = msg.wParam.0 as u16;
                let step = match key {
                    k if ctrl && k == VK_TAB.0 => Some(!shift),
                    k if ctrl && k == VK_NEXT.0 => Some(true),
                    k if ctrl && k == VK_PRIOR.0 => Some(false),
                    _ => None,
                };
                if let Some(forward) = step {
                    cycle_tab(hwnd, forward);
                    continue;
                }
            }
            // Enter in the device tree belongs to the tree (listen or send),
            // not to the dialog manager.
            let tree_enter = msg.message == WM_KEYDOWN
                && msg.wParam.0
                    == usize::from(windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN.0)
                && crate::remote::is_tree(hwnd, msg.hwnd);
            let handled = !tree_enter
                && match owned {
                    Some(p) => IsDialogMessageW(p, &msg).as_bool(),
                    None => IsDialogMessageW(hwnd, &msg).as_bool(),
                };
            if !handled {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    with_state(|s| {
        let _ = s.hwnd;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every control of the window has an id of its own: pages are shown
    /// and hidden by id, so two controls sharing one would leave one of
    /// them on the wrong page.
    #[test]
    fn control_ids_are_unique() {
        let mut ids: Vec<i32> = [
            Page::SignIn,
            Page::Devices,
            Page::Streams,
            Page::Accounts,
            Page::Settings,
        ]
        .iter()
        .flat_map(|p| p.controls().iter().copied())
        .chain([ID_LOG, ID_EXIT, ID_TABS])
        .collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "a control id is used twice: {ids:?}");
    }

    /// Each page's first control is one of its own.
    #[test]
    fn first_controls_belong_to_their_pages() {
        for p in [
            Page::SignIn,
            Page::Devices,
            Page::Streams,
            Page::Accounts,
            Page::Settings,
        ] {
            assert!(p.controls().contains(&p.first()), "{p:?}");
        }
    }
}
