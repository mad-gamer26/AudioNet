//! [`OutputMute`]: AudioNet's own mute of one output device (a private,
//! muting process tap). See the crate documentation.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::NonNull;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioHardwareCreateProcessTap, AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};
use objc2_foundation::{NSArray, NSNumber, NSString};

/// Keeps one output device silent (except for AudioNet's own sound) for as
/// long as it lives. Dropping it lets the sound play again.
#[derive(Debug)]
pub struct OutputMute {
    tap: AudioObjectID,
    /// AudioNet's own Core Audio process object when the tap was made (0:
    /// it had none yet, so it could not be left out).
    excluded: AudioObjectID,
}

impl OutputMute {
    /// Mutes the output device whose Core Audio UID is `device_uid` (what
    /// cpal uses as a device id on macOS). Needs macOS 14.2 or later and
    /// the "System Audio Recording" permission (as recording it does).
    pub fn new(device_uid: &str) -> Result<Self, String> {
        let excluded = own_process_object();
        let ids: Vec<Retained<NSNumber>> = if excluded != 0 {
            vec![NSNumber::new_u32(excluded)]
        } else {
            Vec::new()
        };
        let processes = NSArray::from_retained_slice(&ids);
        let uid = NSString::from_str(device_uid);
        // SAFETY: an Objective-C initializer on a fresh allocation, with
        // valid references to an NSArray of NSNumbers and an NSString.
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
        if status != 0 || tap == 0 {
            return Err(format!(
                "Core Audio could not mute the output (system audio needs macOS 14.2 or later and the System Audio Recording permission; error {status})"
            ));
        }
        Ok(Self { tap, excluded })
    }

    /// True when AudioNet now has a Core Audio process object it did not
    /// have when this mute was made (it started playing sound since): the
    /// mute should be made again, so that sound is left out.
    pub fn own_sound_changed(&self) -> bool {
        own_process_object() != self.excluded
    }
}

impl Drop for OutputMute {
    fn drop(&mut self) {
        // SAFETY: `tap` is the tap this value created and has not
        // destroyed. The status is not checked: nothing can be done here.
        let _ = unsafe { AudioHardwareDestroyProcessTap(self.tap) };
    }
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
