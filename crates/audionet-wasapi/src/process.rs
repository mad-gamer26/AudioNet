//! Process helpers for the `audionet` program's Windows side. They live in
//! this crate because it is the one allowed `unsafe` Win32 calls.

use windows::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

/// Stops this process's standard input, output and error handles from being
/// inherited by processes it starts later.
///
/// `audionet node run --background` starts the device as a separate process
/// with its own log file as output. Windows otherwise hands that process
/// every inheritable handle, including the output pipe of whatever started
/// `audionet` (a script capturing its output), which then waits until the
/// background device stops. Handles that are missing or cannot be changed
/// are left alone: at worst the caller waits, as before.
pub fn stop_inheriting_std_handles() {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        clear_inherit(which);
    }
}

fn clear_inherit(which: STD_HANDLE) {
    // SAFETY: GetStdHandle takes a constant selector and only reads the
    // process's standard handle table; it returns an error for a missing
    // handle, which is skipped.
    let Ok(handle) = (unsafe { GetStdHandle(which) }) else {
        return;
    };
    // SAFETY: `handle` is this process's own standard handle, valid for the
    // process's lifetime; clearing HANDLE_FLAG_INHERIT only changes whether
    // child processes receive a copy, not the handle itself. No other thread
    // is starting processes at this point in the command-line program.
    let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
}
