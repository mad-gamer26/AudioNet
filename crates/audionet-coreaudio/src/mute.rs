//! [`OutputMute`]: AudioNet's own mute of one output device (a private,
//! muting process tap, kept running). See the crate documentation.
//!
//! A tap that is only created mutes nothing: the sound still played (the
//! first 1.2.1 test build did that). The tap therefore goes into a private
//! aggregate device whose input runs, with an IO procedure that ignores
//! what it is given, for as long as the mute lasts.

use std::ffi::{CStr, c_void};
use std::mem::size_of;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, Message};
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubTapUIDKey, kAudioTapPropertyUID,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

/// Tells apart the aggregate devices of one process.
static INSTANCE: AtomicU32 = AtomicU32::new(0);

/// Keeps one output device silent (except for AudioNet's own sound) for as
/// long as it lives. Dropping it lets the sound play again.
#[derive(Debug)]
pub struct OutputMute {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
    /// AudioNet's own Core Audio process object when the tap was made (0:
    /// it had none yet, so it could not be left out).
    excluded: AudioObjectID,
}

// SAFETY: the fields are Core Audio object ids and a function pointer.
// Core Audio's hardware calls may be made from any thread, and the value
// is only used (made, dropped) by the one thread that owns it.
unsafe impl Send for OutputMute {}

impl OutputMute {
    /// Mutes the output device whose Core Audio UID is `device_uid` (what
    /// cpal uses as a device id on macOS, after "coreaudio:"). Needs macOS
    /// 14.2 or later and the "System Audio Recording" permission (as
    /// recording it does). Blocks briefly (Core Audio): never call it from
    /// an audio callback.
    pub fn new(device_uid: &str) -> Result<Self, String> {
        let excluded = own_process_object();
        let mut mute = OutputMute {
            tap: 0,
            aggregate: 0,
            io_proc: None,
            excluded,
        };
        // On an error, dropping `mute` undoes the steps already done.
        mute.tap = create_tap(device_uid, excluded)?;
        mute.aggregate = create_aggregate(mute.tap)?;
        let mut io_proc: AudioDeviceIOProcID = None;
        // SAFETY: `aggregate` is the device just made; `discard` has the
        // AudioDeviceIOProc signature and uses no client data; `io_proc`
        // is a writable local.
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                mute.aggregate,
                Some(discard),
                std::ptr::null_mut(),
                NonNull::from(&mut io_proc),
            )
        };
        check(status, "could not start reading the muting tap")?;
        mute.io_proc = io_proc;
        // SAFETY: the device and IO procedure id were just made.
        let status = unsafe { AudioDeviceStart(mute.aggregate, mute.io_proc) };
        check(status, "could not start the muting tap")?;
        Ok(mute)
    }

    /// True when AudioNet now has a Core Audio process object it did not
    /// have when this mute was made (it started using audio since): the
    /// mute should be made again, so AudioNet's own sound is left out.
    pub fn own_sound_changed(&self) -> bool {
        own_process_object() != self.excluded
    }
}

impl Drop for OutputMute {
    fn drop(&mut self) {
        // SAFETY: each id is one this value made and has not destroyed (0
        // or None: never made). Statuses are not checked: nothing can be
        // done about a failure here.
        unsafe {
            if self.io_proc.is_some() {
                let _ = AudioDeviceStop(self.aggregate, self.io_proc);
                let _ = AudioDeviceDestroyIOProcID(self.aggregate, self.io_proc);
            }
            if self.aggregate != 0 {
                let _ = AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            if self.tap != 0 {
                let _ = AudioHardwareDestroyProcessTap(self.tap);
            }
        }
    }
}

/// The IO procedure that keeps the muting tap running. Real-time: it
/// ignores the tap's audio and does nothing else.
unsafe extern "C-unwind" fn discard(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    _input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    _client: *mut c_void,
) -> i32 {
    0
}

fn check(status: i32, what: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("Core Audio {what} (error {status})"))
    }
}

/// A private tap on the device of every process but `excluded`, muting
/// them while it runs.
fn create_tap(device_uid: &str, excluded: AudioObjectID) -> Result<AudioObjectID, String> {
    let ids: Vec<Retained<NSNumber>> = if excluded != 0 {
        vec![NSNumber::new_u32(excluded)]
    } else {
        Vec::new()
    };
    let processes = NSArray::from_retained_slice(&ids);
    let uid = NSString::from_str(device_uid);
    // SAFETY: an Objective-C initializer on a fresh allocation, with valid
    // references to an NSArray of NSNumbers and an NSString.
    let description = unsafe {
        CATapDescription::initExcludingProcesses_andDeviceUID_withStream(
            CATapDescription::alloc(),
            &processes,
            &uid,
            0,
        )
    };
    // SAFETY: plain property setters on the description just made.
    unsafe {
        description.setMuteBehavior(CATapMuteBehavior::Muted);
        description.setName(&NSString::from_str("AudioNet output mute"));
        description.setPrivate(true);
    }
    let mut tap: AudioObjectID = 0;
    // SAFETY: the description is valid, and `tap` is a writable
    // AudioObjectID for the new tap's id.
    let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap) };
    check(
        status,
        "could not create the muting tap (this needs macOS 14.2 or later and the System Audio Recording permission)",
    )?;
    Ok(tap)
}

fn key(k: &CStr) -> Retained<NSString> {
    NSString::from_str(&k.to_string_lossy())
}

fn object<T: AsRef<AnyObject>>(o: &T) -> Retained<AnyObject> {
    let any: &AnyObject = o.as_ref();
    any.retain()
}

/// A private aggregate device holding only the tap `tap`, started with the
/// device (as cpal makes its recording one).
fn create_aggregate(tap: AudioObjectID) -> Result<AudioObjectID, String> {
    let tap_uid = tap_uid(tap)?;
    let sub_tap: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_retained_objects(&[&*key(kAudioSubTapUIDKey)], &[object(&*tap_uid)]);
    let taps = NSArray::from_retained_slice(&[sub_tap]);
    let n = INSTANCE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let uid = NSString::from_str(&format!("org.audionet.output-mute.{pid}.{n}"));
    let name = NSString::from_str("AudioNet output mute");
    let yes = NSNumber::new_bool(true);
    let keys = [
        key(kAudioAggregateDeviceUIDKey),
        key(kAudioAggregateDeviceNameKey),
        key(kAudioAggregateDeviceIsPrivateKey),
        key(kAudioAggregateDeviceTapListKey),
        key(kAudioAggregateDeviceTapAutoStartKey),
    ];
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let description: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_retained_objects(
            &key_refs,
            &[
                object(&*uid),
                object(&*name),
                object(&*yes),
                object(&*taps),
                object(&*yes),
            ],
        );
    let dictionary: &NSDictionary<NSString, AnyObject> = &description;
    // SAFETY: NSDictionary is toll-free bridged with CFDictionary: the same
    // object, borrowed for the call only.
    let cf: &CFDictionary = unsafe {
        &*(dictionary as *const NSDictionary<NSString, AnyObject>).cast::<CFDictionary>()
    };
    let mut aggregate: AudioObjectID = 0;
    // SAFETY: a valid description dictionary and a writable id.
    let status = unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut aggregate)) };
    check(status, "could not create the muting device")?;
    Ok(aggregate)
}

/// The UID of the tap `tap` (kAudioTapPropertyUID).
fn tap_uid(tap: AudioObjectID) -> Result<Retained<NSString>, String> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioTapPropertyUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut uid: *mut NSString = std::ptr::null_mut();
    let mut size = size_of::<*mut NSString>() as u32;
    // SAFETY: the property is a CFString (toll-free bridged with NSString)
    // returned at +1 into `uid`, its size given in `size`; both are locals
    // that outlive the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut uid).cast(),
        )
    };
    check(status, "could not read the muting tap")?;
    // SAFETY: on success `uid` is an owned (+1) string, or null.
    unsafe { Retained::from_raw(uid) }.ok_or_else(|| "Core Audio gave the muting tap no id".into())
}

/// AudioNet's own Core Audio process object, or 0 when it has none (it has
/// not used audio yet) or it cannot be read.
fn own_process_object() -> AudioObjectID {
    let pid = std::process::id() as i32;
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut object: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    // SAFETY: the qualifier is the pid (an i32, its size given); the
    // result is one AudioObjectID, its size given in `size`. All pointers
    // are to locals that outlive the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            size_of::<i32>() as u32,
            (&pid as *const i32).cast::<c_void>(),
            NonNull::from(&mut size),
            NonNull::from(&mut object).cast(),
        )
    };
    if status == 0 { object } else { 0 }
}
