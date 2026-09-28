//! Handing over from the running copy to a freshly installed one.
//!
//! The old copy creates a named event, starts the new copy with
//! `--after-update <old pid>`, and waits for the event. The new copy sets
//! the event as soon as it runs (proving the new program starts), then
//! waits for the old copy to exit before creating its window, since only
//! one copy may run. If the event is not set in time, the old copy rolls
//! the files back and keeps running.

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, OpenEventW, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent,
    WaitForSingleObject,
};
use windows::core::HSTRING;

fn event_name(old_pid: u32) -> HSTRING {
    HSTRING::from(format!("Local\\AudioNetUpdateStarted-{old_pid}"))
}

/// A kernel handle closed on drop.
pub struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW/OpenProcess and is
        // closed only here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Old copy: the event the new copy will set.
pub fn started_event() -> Option<Handle> {
    // SAFETY: an unnamed-security, manual-reset, initially unset event
    // under a name unique to this process.
    unsafe { CreateEventW(None, true, false, &event_name(std::process::id())) }
        .ok()
        .map(Handle)
}

/// Old copy: waits for the new copy to report that it runs.
pub fn wait_started(event: &Handle, timeout: Duration) -> bool {
    // SAFETY: waiting on an event handle we own.
    unsafe { WaitForSingleObject(event.0, timeout.as_millis() as u32) == WAIT_OBJECT_0 }
}

/// New copy: tells the old copy that it runs, then waits (bounded) for
/// the old copy to exit.
pub fn take_over(old_pid: u32, timeout: Duration) {
    // SAFETY: opening the old copy's event by name and setting it; opening
    // the old process only to wait for it; handles closed by `Handle`.
    unsafe {
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, &event_name(old_pid)) {
            let event = Handle(event);
            let _ = SetEvent(event.0);
        }
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, old_pid) {
            let process = Handle(process);
            let _ = WaitForSingleObject(process.0, timeout.as_millis() as u32);
        }
    }
}
