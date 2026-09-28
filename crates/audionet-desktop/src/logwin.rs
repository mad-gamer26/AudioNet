//! The status log: every event in words, kept here and shown in a window
//! of its own (the main window's "Status log…" button), so it does not fill
//! the main window. Important lines are also spoken as they happen (see
//! `ui::announce`), whether or not the log is open.
//!
//! The window holds a read-only multi-line edit named "Status log" (screen
//! readers move through it line by line; it opens at the newest line),
//! Copy, and Close (also Escape). Focus returns to where it was when the
//! window closes. The main window's message loop gives it keyboard
//! navigation (`IsDialogMessageW` with `handle()`).

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{EM_REPLACESEL, EM_SCROLLCARET, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};

use crate::ui::{self, control};

const ID_LABEL: i32 = 401;
const ID_TEXT: i32 = 402;
const ID_COPY: i32 = 403;
/// Escape sends IDCANCEL through `IsDialogMessageW`.
const ID_CLOSE: i32 = IDCANCEL.0;
/// The log keeps about this much text (oldest lines go first).
const MAX_CHARS: usize = 64_000;

thread_local! {
    static TEXT: RefCell<String> = const { RefCell::new(String::new()) };
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static OWNER: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static FONT: Cell<HFONT> = const { Cell::new(HFONT(std::ptr::null_mut())) };
    static RETURN_FOCUS: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

/// The status log window, if open (for the message loop's keyboard
/// handling and for announcements).
pub fn handle() -> Option<HWND> {
    let h = WINDOW.with(Cell::get);
    crate::owned::alive(h).then_some(h)
}

/// Adds a line to the log (and to the window, if open).
pub fn append(line: &str) {
    let line = format!("{line}\r\n");
    let trimmed = TEXT.with(|t| {
        let mut t = t.borrow_mut();
        t.push_str(&line);
        if t.len() > MAX_CHARS {
            // Drop whole lines from the start.
            let cut = t.len() - MAX_CHARS * 3 / 4;
            let at = t[cut..].find('\n').map_or(cut, |i| cut + i + 1);
            t.drain(..at);
            true
        } else {
            false
        }
    });
    if let Some(h) = handle() {
        if trimmed {
            ui::set_text(h, ID_TEXT, &all());
            scroll_to_end(h);
        } else {
            append_to_edit(h, &line);
        }
    }
}

/// The whole log.
pub fn all() -> String {
    TEXT.with(|t| t.borrow().clone())
}

fn append_to_edit(h: HWND, line: &str) {
    let edit = ui::item(h, ID_TEXT);
    let text = HSTRING::from(line);
    // SAFETY: standard edit-control messages on our own child control; the
    // string outlives SendMessageW.
    unsafe {
        let end = GetWindowTextLengthW(edit).max(0) as usize;
        let _ = SendMessageW(
            edit,
            EM_SETSEL,
            Some(WPARAM(end)),
            Some(LPARAM(end as isize)),
        );
        let _ = SendMessageW(
            edit,
            EM_REPLACESEL,
            Some(WPARAM(0)),
            Some(LPARAM(text.as_ptr() as isize)),
        );
    }
}

/// Puts the caret on the last line, so reading starts at the newest event.
fn scroll_to_end(h: HWND) {
    let edit = ui::item(h, ID_TEXT);
    // SAFETY: standard edit-control messages on our own child control.
    unsafe {
        let end = GetWindowTextLengthW(edit).max(0) as usize;
        let _ = SendMessageW(
            edit,
            EM_SETSEL,
            Some(WPARAM(end)),
            Some(LPARAM(end as isize)),
        );
        let _ = SendMessageW(edit, EM_SCROLLCARET, None, None);
    }
}

/// Opens the status log (or brings it forward), owned by the main window.
pub fn open(owner: HWND) {
    if let Some(h) = handle() {
        crate::owned::bring_forward(h);
        return;
    }
    // SAFETY: reading the focused window of our own thread.
    RETURN_FOCUS.with(|f| f.set(unsafe { GetFocus() }));
    OWNER.with(|o| o.set(owner));
    let Some((hwnd, scale, font)) = crate::owned::create(
        owner,
        "StatusLog",
        "AudioNet Status Log",
        (640, 440),
        Some(wndproc),
    ) else {
        return;
    };
    WINDOW.with(|w| w.set(hwnd));
    FONT.with(|f| f.set(font));
    let (x, full) = (16, 608);
    control(
        hwnd,
        w!("STATIC"),
        "Status &log:",
        WINDOW_STYLE(0),
        WINDOW_EX_STYLE(0),
        ID_LABEL,
        (x, 12, full, 20),
        scale,
        font,
    );
    control(
        hwnd,
        w!("EDIT"),
        "",
        WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32)
            | WS_VSCROLL
            | WS_TABSTOP,
        WS_EX_CLIENTEDGE,
        ID_TEXT,
        (x, 34, full, 350),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "&Copy",
        WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
        WINDOW_EX_STYLE(0),
        ID_COPY,
        (x, 396, 110, 30),
        scale,
        font,
    );
    control(
        hwnd,
        w!("BUTTON"),
        "Close",
        WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
        WINDOW_EX_STYLE(0),
        ID_CLOSE,
        (x + full - 110, 396, 110, 30),
        scale,
        font,
    );
    // SAFETY: subclassing our own edit control (comctl32 v6); the
    // procedure is a plain function that lives for the whole program.
    unsafe {
        let _ = windows::Win32::UI::Shell::SetWindowSubclass(
            ui::item(hwnd, ID_TEXT),
            Some(log_text_keys),
            1,
            0,
        );
    }
    ui::set_text(hwnd, ID_TEXT, &all());
    scroll_to_end(hwnd);
    // SAFETY: showing our window and focusing the log.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(ui::item(hwnd, ID_TEXT)));
    }
}

/// The log's edit control asks for no Tab, Escape or Enter: a multi-line
/// edit otherwise claims them (to type a tab or a new line), so Tab would
/// stay in the log instead of moving to Copy, and Escape would not close
/// the window. It is read-only, so it has no use for them; arrow keys and
/// the rest still reach it.
extern "system" fn log_text_keys(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN, VK_TAB};
    // SAFETY: default processing by the edit control's own procedure.
    let result = unsafe { windows::Win32::UI::Shell::DefSubclassProc(hwnd, msg, wparam, lparam) };
    if msg != WM_GETDLGCODE {
        return result;
    }
    let mut code = result.0 as u32 & !DLGC_WANTTAB;
    // For a key, lParam points to its message (null when asked in general).
    let key = if lparam.0 == 0 {
        0
    } else {
        // SAFETY: for WM_GETDLGCODE a non-null lParam is a valid MSG for
        // the duration of the call.
        unsafe { (*(lparam.0 as *const MSG)).wParam.0 as u16 }
    };
    if key == VK_TAB.0 || key == VK_ESCAPE.0 || key == VK_RETURN.0 {
        code &= !DLGC_WANTALLKEYS;
    }
    LRESULT(code as isize)
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
            match (wparam.0 & 0xFFFF) as i32 {
                ID_COPY => {
                    ui::copy_to_clipboard(hwnd, &all());
                    ui::announce(hwnd, "Status copied to the clipboard.");
                }
                ID_CLOSE => close(hwnd),
                _ => {}
            }
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
                if crate::owned::alive(back) && IsWindowVisible(owner).as_bool() {
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
        _ => crate::owned::default(hwnd, msg, wparam, lparam),
    }
}
