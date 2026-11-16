//! A device's sound settings as macOS has them (the ones in Control Center
//! and System Settings): volume, mute, and the default input and output;
//! changing them when the account asks; and noticing when they change, so
//! the account sees it at once.
//!
//! Devices are named by their Core Audio UID (what cpal uses after
//! "coreaudio:"). Volume is the device's own scalar (0.0 to 1.0, as its
//! slider): the main element when the device has one, else the average of
//! its first two channels (set together, so the balance stays). macOS has
//! no way to turn a device off, so that is never offered.
//!
//! Threads: every call blocks briefly in Core Audio, so callers use a
//! control thread, never an audio callback. Change notices arrive on Core
//! Audio's own notification thread and only call the waker.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectHasProperty, AudioObjectID, AudioObjectIsPropertySettable,
    AudioObjectPropertyAddress, AudioObjectSetPropertyData, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyMute, kAudioDevicePropertyVolumeScalar,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDefaultSystemOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_foundation::NSString;

/// Called when any device's settings change (from Core Audio's thread).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// One device's settings in one direction (its input or its output).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// 0.0 to 1.0; `None` when the device has no volume of its own.
    pub volume: Option<f32>,
    /// `None` when the device has no mute.
    pub muted: Option<bool>,
    pub can_set_volume: bool,
    pub can_mute: bool,
}

fn address(selector: u32, scope: u32, element: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    }
}

fn scope(input: bool) -> u32 {
    if input {
        kAudioObjectPropertyScopeInput
    } else {
        kAudioObjectPropertyScopeOutput
    }
}

fn has(object: AudioObjectID, a: &AudioObjectPropertyAddress) -> bool {
    // SAFETY: `a` is a valid address for the call's duration.
    unsafe { AudioObjectHasProperty(object, NonNull::from(a)) }
}

fn settable(object: AudioObjectID, a: &AudioObjectPropertyAddress) -> bool {
    let mut out: u8 = 0;
    // SAFETY: `a` and `out` are valid locals for the call's duration.
    let status =
        unsafe { AudioObjectIsPropertySettable(object, NonNull::from(a), NonNull::from(&mut out)) };
    status == 0 && out != 0
}

/// Reads a plain value (`f32`, `u32`) property.
fn get<T: Copy + Default>(object: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<T> {
    let mut value = T::default();
    let mut size = size_of::<T>() as u32;
    // SAFETY: `value` is a writable `T` of `size` bytes; no qualifier.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == 0 && size as usize == size_of::<T>()).then_some(value)
}

/// Writes a plain value property; the error is Core Audio's status.
fn set<T: Copy>(
    object: AudioObjectID,
    a: &AudioObjectPropertyAddress,
    value: T,
) -> Result<(), i32> {
    // SAFETY: `value` is a readable `T` of the size given; no qualifier.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            NonNull::from(a),
            0,
            std::ptr::null(),
            size_of::<T>() as u32,
            NonNull::from(&value).cast(),
        )
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

/// Every audio device's object id.
fn devices() -> Vec<AudioObjectID> {
    let system = kAudioObjectSystemObject as AudioObjectID;
    let a = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let mut size: u32 = 0;
    // SAFETY: `a` and `size` are valid locals.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            system,
            NonNull::from(&a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    // SAFETY: `ids` has room for `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr()).expect("non-null").cast(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate(size as usize / size_of::<AudioObjectID>());
    ids
}

/// A device's Core Audio UID.
fn uid_of(device: AudioObjectID) -> Option<String> {
    let a = address(
        kAudioDevicePropertyDeviceUID,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let mut value: *mut NSString = std::ptr::null_mut();
    let mut size = size_of::<*mut NSString>() as u32;
    // SAFETY: `value` is a writable pointer-sized local; Core Audio returns
    // a CFString it retained for us (toll-free bridged with NSString).
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    if status != 0 || value.is_null() {
        return None;
    }
    // SAFETY: a +1 reference to an NSString-compatible object: ours to release.
    let s = unsafe { Retained::from_raw(value) }?;
    Some(s.to_string())
}

/// The device with Core Audio UID `uid`.
fn device(uid: &str) -> Result<AudioObjectID, String> {
    devices()
        .into_iter()
        .find(|d| uid_of(*d).as_deref() == Some(uid))
        .ok_or_else(|| "That sound device isn't connected any more.".to_owned())
}

/// The elements carrying a device's volume (or mute) in one direction: the
/// main one, else its first two channels.
fn elements(device: AudioObjectID, selector: u32, input: bool) -> Vec<u32> {
    let main = address(selector, scope(input), kAudioObjectPropertyElementMain);
    if has(device, &main) {
        return vec![kAudioObjectPropertyElementMain];
    }
    (1..=2)
        .filter(|e| has(device, &address(selector, scope(input), *e)))
        .collect()
}

/// A device's settings in one direction, `None` if the device isn't found.
pub fn settings(uid: &str, input: bool) -> Option<Settings> {
    let d = device(uid).ok()?;
    let s = scope(input);
    let vol = elements(d, kAudioDevicePropertyVolumeScalar, input);
    let volumes: Vec<f32> = vol
        .iter()
        .filter_map(|e| get::<f32>(d, &address(kAudioDevicePropertyVolumeScalar, s, *e)))
        .collect();
    let mute = elements(d, kAudioDevicePropertyMute, input);
    let muted = mute
        .first()
        .and_then(|e| get::<u32>(d, &address(kAudioDevicePropertyMute, s, *e)))
        .map(|m| m != 0);
    Some(Settings {
        volume: (!volumes.is_empty()).then(|| volumes.iter().sum::<f32>() / volumes.len() as f32),
        muted,
        can_set_volume: vol
            .iter()
            .any(|e| settable(d, &address(kAudioDevicePropertyVolumeScalar, s, *e))),
        can_mute: mute
            .iter()
            .any(|e| settable(d, &address(kAudioDevicePropertyMute, s, *e))),
    })
}

fn failed(what: &str, status: i32) -> String {
    format!("macOS didn't {what} (error {status}).")
}

/// Sets a device's volume (0.0 to 1.0) in one direction.
pub fn set_volume(uid: &str, input: bool, volume: f32) -> Result<(), String> {
    let d = device(uid)?;
    let es = elements(d, kAudioDevicePropertyVolumeScalar, input);
    if es.is_empty() {
        return Err("This sound device has no volume of its own.".into());
    }
    for e in es {
        set(
            d,
            &address(kAudioDevicePropertyVolumeScalar, scope(input), e),
            volume.clamp(0.0, 1.0),
        )
        .map_err(|s| failed("change the volume", s))?;
    }
    Ok(())
}

/// Mutes or unmutes a device in one direction.
pub fn set_muted(uid: &str, input: bool, muted: bool) -> Result<(), String> {
    let d = device(uid)?;
    let es = elements(d, kAudioDevicePropertyMute, input);
    if es.is_empty() {
        return Err("This sound device can't be muted.".into());
    }
    for e in es {
        set(
            d,
            &address(kAudioDevicePropertyMute, scope(input), e),
            u32::from(muted),
        )
        .map_err(|s| failed(if muted { "mute it" } else { "unmute it" }, s))?;
    }
    Ok(())
}

fn default_selector(input: bool) -> u32 {
    if input {
        kAudioHardwarePropertyDefaultInputDevice
    } else {
        kAudioHardwarePropertyDefaultOutputDevice
    }
}

/// The UID of the default input or output.
pub fn default_device(input: bool) -> Option<String> {
    let a = address(
        default_selector(input),
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    get::<AudioObjectID>(kAudioObjectSystemObject as AudioObjectID, &a).and_then(uid_of)
}

/// Makes a device the default input or output (as choosing it in System
/// Settings → Sound does; an output also plays the alert sounds when they
/// follow the default, as they do unless the person chose otherwise).
pub fn set_default(uid: &str, input: bool) -> Result<(), String> {
    let d = device(uid)?;
    let system = kAudioObjectSystemObject as AudioObjectID;
    let a = address(
        default_selector(input),
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let alerts = address(
        kAudioHardwarePropertyDefaultSystemOutputDevice,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let alerts_followed =
        !input && get::<AudioObjectID>(system, &alerts) == get::<AudioObjectID>(system, &a);
    set(system, &a, d).map_err(|s| failed("make it the default", s))?;
    if alerts_followed {
        let _ = set(system, &alerts, d);
    }
    Ok(())
}

/// Whoever wants to hear of changes (the engine's connection; replaced by
/// a newer one).
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);
/// The objects and properties already listened to.
static LISTENING: Mutex<Vec<(AudioObjectID, u32, u32, u32)>> = Mutex::new(Vec::new());

/// Core Audio's notice: tell the engine, and listen to new devices.
unsafe extern "C-unwind" fn changed(
    object: AudioObjectID,
    count: u32,
    addresses: NonNull<AudioObjectPropertyAddress>,
    _client: *mut c_void,
) -> i32 {
    // SAFETY: Core Audio passes `count` addresses.
    let list = unsafe { std::slice::from_raw_parts(addresses.as_ptr(), count as usize) };
    if object == kAudioObjectSystemObject as AudioObjectID
        && list
            .iter()
            .any(|a| a.mSelector == kAudioHardwarePropertyDevices)
    {
        listen_to_devices();
    }
    let waker = WAKER.lock().ok().and_then(|w| w.clone());
    if let Some(w) = waker {
        w();
    }
    0
}

fn listen(object: AudioObjectID, a: AudioObjectPropertyAddress) {
    let key = (object, a.mSelector, a.mScope, a.mElement);
    let Ok(mut done) = LISTENING.lock() else {
        return;
    };
    if done.contains(&key) || !has(object, &a) {
        return;
    }
    // SAFETY: `changed` has the listener signature and uses no client
    // data; Core Audio copies the address.
    let status = unsafe {
        AudioObjectAddPropertyListener(
            object,
            NonNull::from(&a),
            Some(changed),
            std::ptr::null_mut(),
        )
    };
    if status == 0 {
        done.push(key);
    }
}

fn listen_to_devices() {
    for d in devices() {
        for input in [true, false] {
            for selector in [kAudioDevicePropertyVolumeScalar, kAudioDevicePropertyMute] {
                for e in [kAudioObjectPropertyElementMain, 1, 2] {
                    listen(d, address(selector, scope(input), e));
                }
            }
        }
    }
    // Gone devices' entries are dropped with them (their ids aren't reused
    // while this process runs, and a removed object needs no removal).
    let live = devices();
    if let Ok(mut done) = LISTENING.lock() {
        done.retain(|(o, ..)| *o == kAudioObjectSystemObject as AudioObjectID || live.contains(o));
    }
}

/// Calls `waker` whenever a device's volume or mute changes, the default
/// input or output changes, or a device comes or goes, however it was
/// changed (in Control Center, System Settings, a keyboard's volume keys,
/// another app, or AudioNet). Replaces an earlier waker.
pub fn watch(waker: Waker) {
    if let Ok(mut w) = WAKER.lock() {
        *w = Some(waker);
    }
    let system = kAudioObjectSystemObject as AudioObjectID;
    for selector in [
        kAudioHardwarePropertyDevices,
        kAudioHardwarePropertyDefaultInputDevice,
        kAudioHardwarePropertyDefaultOutputDevice,
    ] {
        listen(
            system,
            address(
                selector,
                kAudioObjectPropertyScopeGlobal,
                kAudioObjectPropertyElementMain,
            ),
        );
    }
    listen_to_devices();
}
