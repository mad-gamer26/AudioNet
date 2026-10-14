//! AudioNet's engine for C and C++ programs: the NVGT plugin
//! (`nvgt-plugin/`), and anything else with a C interface. The header is
//! `include/audionet.h`.
//!
//! A program signs in to an account as a visitor (like the web client and
//! the NVDA add-on: no device is added), lists the account's devices,
//! listens to them and sends to them. Audio can also come to the program
//! (an audio tap: it reads what a stream plays) and from it (an app source:
//! it writes what a stream sends), so a game can play AudioNet sound
//! through its own mixer and send its own.
//!
//! Conventions, for C callers:
//! * Strings in are UTF-8 and NUL-terminated.
//! * Strings out go into the caller's buffer, NUL-terminated. The return
//!   value is the length written; when the buffer is too small, nothing is
//!   written and the return value is minus the size needed (with the NUL).
//! * Objects are numbered handles (never pointers); 0 is "none".
//! * A call that fails returns 0 (or a negative number) and keeps why, in
//!   words, for `an_last_error` on the same thread.
//! * Events arrive as JSON text through `an_client_poll`, never as
//!   callbacks: the program reads them on its own thread.
//! * Nothing here waits for audio; only sign-in and sign-out wait for the
//!   network.
//!
//! The only `unsafe` code is the conversion of C pointers into strings and
//! slices, in the four helpers at the end of this file.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex};

use audionet_ffi::{
    AppSource, AudioTap, Client, Device, Event, EventListener, Output, Source, StreamState,
    VisitorLogin,
};
use serde_json::{Value, json};

/// Events kept for a program that does not read them (oldest dropped).
const MAX_EVENTS: usize = 1000;

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn fail(message: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = message.into());
}

static NEXT_HANDLE: AtomicU32 = AtomicU32::new(1);

fn new_handle() -> u32 {
    NEXT_HANDLE.fetch_add(1, Relaxed)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// MARK: Clients and their events

/// Events waiting for the program, as JSON lines.
#[derive(Default)]
struct Queue(Mutex<VecDeque<String>>);

impl EventListener for Queue {
    fn on_event(&self, event: Event) {
        let mut q = lock(&self.0);
        if q.len() == MAX_EVENTS {
            q.pop_front();
        }
        q.push_back(event_json(&event).to_string());
    }
}

struct ClientEntry {
    client: Arc<Client>,
    events: Arc<Queue>,
}

static CLIENTS: LazyLock<Mutex<HashMap<u32, Arc<ClientEntry>>>> = LazyLock::new(Default::default);
static TAPS: LazyLock<Mutex<HashMap<u32, Arc<AudioTap>>>> = LazyLock::new(Default::default);
static SOURCES: LazyLock<Mutex<HashMap<u32, Arc<AppSource>>>> = LazyLock::new(Default::default);

fn client(handle: u32) -> Option<Arc<ClientEntry>> {
    let found = lock(&CLIENTS).get(&handle).cloned();
    if found.is_none() {
        fail("this AudioNet connection is closed");
    }
    found
}

fn source_json(s: &Source) -> Value {
    json!({ "id": s.id, "name": s.name, "is_input": s.is_input, "is_default": s.is_default })
}

fn output_json(o: &Output) -> Value {
    json!({ "id": o.id, "name": o.name, "is_default": o.is_default })
}

fn device_json(d: &Device) -> Value {
    json!({
        "node_id": d.node_id,
        "name": d.name,
        "online": d.online,
        "sharing": d.sharing,
        "platform": d.platform,
        "sources": d.sources.iter().map(source_json).collect::<Vec<_>>(),
        "outputs": d.outputs.iter().map(output_json).collect::<Vec<_>>(),
    })
}

/// An event as JSON: `{"type": ..., ...}` with every field present.
fn event_json(e: &Event) -> Value {
    match e {
        Event::Connected => json!({ "type": "connected" }),
        Event::Disconnected { reason } => json!({ "type": "disconnected", "reason": reason }),
        Event::Status { text } => json!({ "type": "status", "text": text }),
        Event::Devices { devices } => json!({
            "type": "devices",
            "devices": devices.iter().map(device_json).collect::<Vec<_>>(),
        }),
        Event::DeviceChanged { device } => {
            json!({ "type": "device", "device": device_json(device) })
        }
        Event::Stream {
            session_id,
            state,
            detail,
        } => json!({
            "type": "stream",
            "session_id": session_id,
            "state": match state {
                StreamState::Starting => "starting",
                StreamState::Active => "active",
                StreamState::Ended => "ended",
                StreamState::Failed => "failed",
            },
            "detail": detail,
        }),
        Event::StreamEnded { session_id, reason } => {
            json!({ "type": "stream_ended", "session_id": session_id, "reason": reason })
        }
        Event::Diagnostics { session_id, text } => {
            json!({ "type": "diagnostics", "session_id": session_id, "text": text })
        }
        Event::ServerProblem { message } => json!({ "type": "server_problem", "message": message }),
        Event::Stopped { error } => json!({ "type": "stopped", "error": error }),
    }
}

/// The engine's version ("1.2.3").
#[unsafe(no_mangle)]
pub extern "C" fn an_version(out: *mut c_char, cap: i32) -> i32 {
    write_out(&audionet_ffi::engine_version(), out, cap)
}

/// Why the last call on this thread failed, in words.
#[unsafe(no_mangle)]
pub extern "C" fn an_last_error(out: *mut c_char, cap: i32) -> i32 {
    let text = LAST_ERROR.with(|e| e.borrow().clone());
    write_out(&text, out, cap)
}

/// Signs in to an account as a visitor with the password (used once, never
/// kept) and writes the session token into `out` (a secret: keep it only as
/// long as needed). Waits for the network.
#[unsafe(no_mangle)]
pub extern "C" fn an_visitor_sign_in(
    server: *const c_char,
    username: *const c_char,
    password: *const c_char,
    out_token: *mut c_char,
    cap: i32,
) -> i32 {
    let (Some(server), Some(username), Some(password)) =
        (cstr(server), cstr(username), cstr(password))
    else {
        fail("the server address, account name and password are needed");
        return 0;
    };
    match audionet_ffi::visitor_sign_in(server, username, password) {
        Ok(login) => write_out(&login.token, out_token, cap),
        Err(e) => {
            fail(format!("Signing in failed: {e}"));
            0
        }
    }
}

/// Ends a visitor's session on the server. Waits for the network.
#[unsafe(no_mangle)]
pub extern "C" fn an_visitor_sign_out(server: *const c_char, token: *const c_char) -> i32 {
    let (Some(server), Some(token)) = (cstr(server), cstr(token)) else {
        fail("the server address and session are needed");
        return 0;
    };
    let login = VisitorLogin {
        server_url: server,
        username: String::new(),
        token,
    };
    match audionet_ffi::visitor_sign_out(login) {
        Ok(()) => 1,
        Err(e) => {
            fail(format!("Signing out failed: {e}"));
            0
        }
    }
}

/// Connects to an account as a visitor (with the token from
/// `an_visitor_sign_in`) and starts at once. `app_name` names the program
/// ("My Game"). Returns a handle, or 0.
#[unsafe(no_mangle)]
pub extern "C" fn an_client_create(
    server: *const c_char,
    username: *const c_char,
    token: *const c_char,
    app_name: *const c_char,
) -> u32 {
    let (Some(server), Some(username), Some(token)) = (cstr(server), cstr(username), cstr(token))
    else {
        fail("the server address, account name and session are needed");
        return 0;
    };
    let app_name = cstr(app_name).unwrap_or_else(|| "AudioNet for NVGT".into());
    let events = Arc::new(Queue::default());
    let login = VisitorLogin {
        server_url: server,
        username,
        token,
    };
    let client = Client::visitor(
        login,
        app_name,
        Arc::clone(&events) as Arc<dyn EventListener>,
    );
    client.start();
    let handle = new_handle();
    lock(&CLIENTS).insert(handle, Arc::new(ClientEntry { client, events }));
    handle
}

/// Disconnects and forgets a client; every stream it started stops.
#[unsafe(no_mangle)]
pub extern "C" fn an_client_destroy(handle: u32) {
    let entry = lock(&CLIENTS).remove(&handle);
    if let Some(entry) = entry {
        entry.client.stop();
    }
}

/// The next event as JSON (see `event_json`): the length written, 0 when
/// there is none, or minus the size needed (the event stays queued).
#[unsafe(no_mangle)]
pub extern "C" fn an_client_poll(handle: u32, out: *mut c_char, cap: i32) -> i32 {
    let Some(entry) = client(handle) else {
        return 0;
    };
    let mut q = lock(&entry.events.0);
    let Some(next) = q.front() else { return 0 };
    let written = write_out(next, out, cap);
    if written >= 0 {
        q.pop_front();
    }
    written
}

/// Asks for the device list again (a "devices" event follows).
#[unsafe(no_mangle)]
pub extern "C" fn an_client_refresh(handle: u32) {
    if let Some(entry) = client(handle) {
        entry.client.refresh_devices();
    }
}

/// Listens to `source_id` of device `node_id`, playing on `output_id` (one
/// of this computer's outputs, or an audio tap's id). Writes the stream's
/// session id; 0 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn an_client_listen(
    handle: u32,
    node_id: *const c_char,
    source_id: *const c_char,
    output_id: *const c_char,
    out_session: *mut c_char,
    cap: i32,
) -> i32 {
    start(
        handle,
        node_id,
        source_id,
        output_id,
        out_session,
        cap,
        true,
    )
}

/// Sends `source_id` (one of this computer's sources, or an app source's
/// id) to `output_id` of device `node_id`. Writes the session id.
#[unsafe(no_mangle)]
pub extern "C" fn an_client_send(
    handle: u32,
    node_id: *const c_char,
    source_id: *const c_char,
    output_id: *const c_char,
    out_session: *mut c_char,
    cap: i32,
) -> i32 {
    start(
        handle,
        node_id,
        source_id,
        output_id,
        out_session,
        cap,
        false,
    )
}

fn start(
    handle: u32,
    node_id: *const c_char,
    source_id: *const c_char,
    output_id: *const c_char,
    out: *mut c_char,
    cap: i32,
    listen: bool,
) -> i32 {
    let Some(entry) = client(handle) else {
        return 0;
    };
    let (Some(node), Some(source), Some(output)) =
        (cstr(node_id), cstr(source_id), cstr(output_id))
    else {
        fail("the device, sound and output are needed");
        return 0;
    };
    let started = if listen {
        entry.client.listen(node, source, output)
    } else {
        entry.client.send(node, source, output)
    };
    match started {
        Ok(session) => write_out(&session, out, cap),
        Err(e) => {
            fail(format!("Could not start: {e}"));
            0
        }
    }
}

/// Stops a stream this client started.
#[unsafe(no_mangle)]
pub extern "C" fn an_client_stop(handle: u32, session_id: *const c_char) {
    if let (Some(entry), Some(id)) = (client(handle), cstr(session_id)) {
        entry.client.stop_stream(id);
    }
}

/// A stream's volume on this computer (0.0 to 1.0, the slider position) and
/// mute.
#[unsafe(no_mangle)]
pub extern "C" fn an_client_set_volume(
    handle: u32,
    session_id: *const c_char,
    volume: f32,
    muted: i32,
) {
    if let (Some(entry), Some(id)) = (client(handle), cstr(session_id)) {
        entry
            .client
            .set_stream_volume(id, volume.clamp(0.0, 1.0), muted != 0);
    }
}

/// This computer's sources and outputs as JSON: `{"sources": [...],
/// "outputs": [...]}`.
#[unsafe(no_mangle)]
pub extern "C" fn an_local_audio(out: *mut c_char, cap: i32) -> i32 {
    match audionet_ffi::local_audio() {
        Ok(a) => {
            let v = json!({
                "sources": a.sources.iter().map(source_json).collect::<Vec<_>>(),
                "outputs": a.outputs.iter().map(output_json).collect::<Vec<_>>(),
            });
            write_out(&v.to_string(), out, cap)
        }
        Err(e) => {
            fail(format!("Could not list this computer's audio: {e}"));
            0
        }
    }
}

// MARK: Audio to the program (taps)

/// An output the program reads audio from: listen with its id as the
/// output. `name` says where the sound goes ("My Game").
#[unsafe(no_mangle)]
pub extern "C" fn an_tap_create(name: *const c_char, sample_rate: u32, channels: u32) -> u32 {
    let name = cstr(name).unwrap_or_else(|| "NVGT".into());
    let handle = new_handle();
    lock(&TAPS).insert(handle, AudioTap::new(name, sample_rate, channels));
    handle
}

#[unsafe(no_mangle)]
pub extern "C" fn an_tap_destroy(handle: u32) {
    lock(&TAPS).remove(&handle);
}

/// The tap's id, to listen with.
#[unsafe(no_mangle)]
pub extern "C" fn an_tap_id(handle: u32, out: *mut c_char, cap: i32) -> i32 {
    let tap = lock(&TAPS).get(&handle).cloned();
    match tap {
        Some(t) => write_out(&t.destination_id(), out, cap),
        None => {
            fail("this audio tap is closed");
            0
        }
    }
}

/// Fills `out` with the next `frames` frames (interleaved floats): what the
/// stream plays, or silence. Returns the frames written. Never waits; the
/// reads set the pace (the stream's drift and depth control follow them).
#[unsafe(no_mangle)]
pub extern "C" fn an_tap_read(handle: u32, out: *mut f32, frames: u32) -> i32 {
    let tap = lock(&TAPS).get(&handle).cloned();
    let Some(tap) = tap else { return 0 };
    let Some(out) = slice_mut(out, frames as usize * tap.channels() as usize) else {
        return 0;
    };
    tap.read_f32(out) as i32
}

/// 1 while a stream plays into the tap.
#[unsafe(no_mangle)]
pub extern "C" fn an_tap_playing(handle: u32) -> i32 {
    lock(&TAPS).get(&handle).is_some_and(|t| t.is_playing()) as i32
}

// MARK: Audio from the program (app sources)

/// A source the program writes audio into: send with its id as the source.
/// `name` says what it is ("Game sound").
#[unsafe(no_mangle)]
pub extern "C" fn an_source_create(name: *const c_char, sample_rate: u32, channels: u32) -> u32 {
    let name = cstr(name).unwrap_or_else(|| "NVGT".into());
    let handle = new_handle();
    lock(&SOURCES).insert(handle, AppSource::new(name, sample_rate, channels));
    handle
}

#[unsafe(no_mangle)]
pub extern "C" fn an_source_destroy(handle: u32) {
    lock(&SOURCES).remove(&handle);
}

/// The source's id, to send with.
#[unsafe(no_mangle)]
pub extern "C" fn an_source_id(handle: u32, out: *mut c_char, cap: i32) -> i32 {
    let source = lock(&SOURCES).get(&handle).cloned();
    match source {
        Some(s) => write_out(&s.source_id(), out, cap),
        None => {
            fail("this audio source is closed");
            0
        }
    }
}

/// Writes `frames` frames (interleaved floats). Returns the frames taken: 0
/// while no stream sends this source, or when they did not fit (dropped).
/// Never waits.
#[unsafe(no_mangle)]
pub extern "C" fn an_source_write(handle: u32, samples: *const f32, frames: u32) -> i32 {
    let source = lock(&SOURCES).get(&handle).cloned();
    let Some(source) = source else { return 0 };
    let Some(samples) = slice(samples, frames as usize * source.channels() as usize) else {
        return 0;
    };
    source.write_f32(samples) as i32
}

/// 1 while a stream sends this source.
#[unsafe(no_mangle)]
pub extern "C" fn an_source_sending(handle: u32) -> i32 {
    lock(&SOURCES).get(&handle).is_some_and(|s| s.is_sending()) as i32
}

// MARK: The C boundary

/// A C string as an owned String (None for null, blank or invalid UTF-8).
fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string that stays valid for
    // the duration of the call (the documented contract of every function
    // here); it is only read.
    let s = unsafe { CStr::from_ptr(p) }.to_str().ok()?;
    (!s.trim().is_empty()).then(|| s.to_owned())
}

/// Writes `text` and a NUL into `out` if `cap` bytes fit it; returns its
/// length, or minus the size needed.
fn write_out(text: &str, out: *mut c_char, cap: i32) -> i32 {
    let needed = text.len() + 1;
    let Ok(cap) = usize::try_from(cap) else {
        return -(needed as i32);
    };
    if out.is_null() || cap < needed {
        return -(needed as i32);
    }
    // SAFETY: `out` points to at least `cap` writable bytes (the caller's
    // contract), and `needed <= cap`; the source is a separate Rust string.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), out.cast::<u8>(), text.len());
        *out.add(text.len()) = 0;
    }
    text.len() as i32
}

/// `len` floats the caller passed in (None for null).
fn slice<'a>(p: *const f32, len: usize) -> Option<&'a [f32]> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller passes `len` readable floats that stay valid and
    // unchanged for the duration of the call.
    Some(unsafe { std::slice::from_raw_parts(p, len) })
}

/// `len` floats the caller lends for writing (None for null).
fn slice_mut<'a>(p: *mut f32, len: usize) -> Option<&'a mut [f32]> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller passes `len` writable floats, not used elsewhere
    // for the duration of the call.
    Some(unsafe { std::slice::from_raw_parts_mut(p, len) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(f: impl Fn(*mut c_char, i32) -> i32) -> (i32, String) {
        let mut buf = vec![0u8; 256];
        let n = f(buf.as_mut_ptr().cast(), buf.len() as i32);
        let s = CStr::from_bytes_until_nul(&buf)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        (n, s)
    }

    #[test]
    fn strings_come_out_with_their_length_or_the_size_needed() {
        let (n, s) = text(|out, cap| write_out("hello", out, cap));
        assert_eq!((n, s.as_str()), (5, "hello"));
        let mut small = [0 as c_char; 5];
        assert_eq!(write_out("hello", small.as_mut_ptr(), 5), -6);
        assert_eq!(write_out("hello", std::ptr::null_mut(), 100), -6);
    }

    #[test]
    fn a_tap_and_a_source_by_handle() {
        let name = c"Game";
        let tap = an_tap_create(name.as_ptr(), 48_000, 2);
        let (_, id) = text(|out, cap| an_tap_id(tap, out, cap));
        assert!(id.starts_with("app:"));
        let mut frames = vec![1.0f32; 960];
        assert_eq!(an_tap_read(tap, frames.as_mut_ptr(), 480), 480);
        assert!(frames.iter().all(|&s| s == 0.0));
        assert_eq!(an_tap_playing(tap), 0);
        an_tap_destroy(tap);
        assert_eq!(an_tap_read(tap, frames.as_mut_ptr(), 480), 0);

        let source = an_source_create(name.as_ptr(), 48_000, 1);
        let (_, id) = text(|out, cap| an_source_id(source, out, cap));
        assert!(id.starts_with("app-source:"));
        // Not sending yet: nothing taken.
        assert_eq!(an_source_write(source, frames.as_ptr(), 480), 0);
        an_source_destroy(source);
    }

    #[test]
    fn events_are_json_with_every_field() {
        let e = event_json(&Event::Stream {
            session_id: "s1".into(),
            state: StreamState::Active,
            detail: "Connected.".into(),
        });
        assert_eq!(e["type"], "stream");
        assert_eq!(e["state"], "active");
        assert_eq!(e["detail"], "Connected.");
        assert_eq!(
            event_json(&Event::Stopped { error: None })["error"],
            Value::Null
        );
    }

    #[test]
    fn an_empty_server_is_refused() {
        let empty = c"";
        let handle = an_client_create(
            empty.as_ptr(),
            empty.as_ptr(),
            empty.as_ptr(),
            empty.as_ptr(),
        );
        assert_eq!(handle, 0);
        assert_eq!(
            text(|out, cap| an_last_error(out, cap)).1,
            "the server address, account name and session are needed"
        );
    }

    #[test]
    fn a_closed_client_says_so() {
        assert_eq!(text(|out, cap| an_client_poll(999_999, out, cap)).0, 0);
        assert_eq!(
            text(|out, cap| an_last_error(out, cap)).1,
            "this AudioNet connection is closed"
        );
    }
}
