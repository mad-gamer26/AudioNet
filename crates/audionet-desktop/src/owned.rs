//! Secondary windows owned by the main window (Settings, the status log):
//! created, sized for the monitor's DPI and centered on the owner the same
//! way, with the system message font.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, HBRUSH, HFONT, WHITE_BRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR};

use crate::ui;

/// The window class for one kind of owned window: the profile's main class
/// name plus `suffix` (so test profiles never find the real app's windows).
fn class_name(suffix: &str) -> HSTRING {
    // SAFETY: the profile's class name is a valid, static wide string.
    let main = unsafe { crate::profile::class_name().to_string() }.unwrap_or_default();
    HSTRING::from(format!("{main}-{suffix}"))
}

/// Creates a titled window of `size` (in 96-DPI units) owned by `owner`,
/// centered on it when it is shown. Not shown yet. Returns the window, its
/// DPI scale and a font for its controls (the caller deletes the font).
pub fn create(
    owner: HWND,
    suffix: &str,
    title: &str,
    size: (i32, i32),
    wndproc: WNDPROC,
) -> Option<(HWND, f32, HFONT)> {
    let class = class_name(suffix);
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
    let ex = WS_EX_CONTROLPARENT | WS_EX_DLGMODALFRAME;
    // SAFETY: registering (once; later calls fail harmlessly) and creating
    // our own top-level window; strings outlive the calls.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            ex,
            PCWSTR(class.as_ptr()),
            &HSTRING::from(title),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            Some(owner),
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?
    };
    // SAFETY: sizing our window for its DPI and centering it on the owner.
    let scale = unsafe {
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: (size.0 as f32 * scale) as i32,
            bottom: (size.1 as f32 * scale) as i32,
        };
        let _ = AdjustWindowRectEx(&mut rect, style, false, ex);
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        let mut owner_rect = RECT::default();
        let (x, y) =
            if IsWindowVisible(owner).as_bool() && GetWindowRect(owner, &mut owner_rect).is_ok() {
                (
                    owner_rect.left + (owner_rect.right - owner_rect.left - w) / 2,
                    owner_rect.top + (owner_rect.bottom - owner_rect.top - h) / 3,
                )
            } else {
                (CW_USEDEFAULT, CW_USEDEFAULT)
            };
        let flags = if x == CW_USEDEFAULT {
            SWP_NOMOVE | SWP_NOZORDER
        } else {
            SWP_NOZORDER
        };
        let _ = SetWindowPos(hwnd, None, x.max(0), y.max(0), w, h, flags);
        scale
    };
    Some((hwnd, scale, ui::message_font(scale)))
}

/// Whether `h` is still a window (an owned window may have been closed).
pub fn alive(h: HWND) -> bool {
    // SAFETY: checking whether a window handle is still a window.
    !h.is_invalid() && unsafe { IsWindow(Some(h)) }.as_bool()
}

/// Brings an open owned window forward.
pub fn bring_forward(h: HWND) {
    // SAFETY: showing our own window.
    unsafe {
        let _ = ShowWindow(h, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(h);
    }
}

/// Default handling (for window procedures).
pub fn default(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: default processing of a message for our own window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
