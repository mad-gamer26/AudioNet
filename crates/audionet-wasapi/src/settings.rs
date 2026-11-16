//! A device's sound settings as Windows has them (the Sound settings and
//! the taskbar's volume): each endpoint's volume and mute
//! (`IAudioEndpointVolume`), the default devices for each role, and
//! endpoints enabled or disabled, changed when the account asks; and
//! notices when any of them changes, so the account sees it at once.
//!
//! Defaults and enabling use `IPolicyConfig`, the interface Windows' own
//! Sound control panel uses for "Set as Default Device" and "Disable"
//! (undocumented, but stable since Windows 7 and used the same way by
//! common tools such as EarTrumpet and SoundSwitch; Windows offers no
//! documented equivalent). No administrator rights are needed.
//!
//! Threading: every call enters COM on the calling thread for its own
//! duration (a control thread, never an audio callback). [`watch`] runs one
//! thread in the multithreaded apartment that holds the registrations;
//! Windows calls the notification objects on its own threads, where they
//! only call the waker.

use std::sync::Arc;
use std::sync::mpsc;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE, EDataFlow, ERole,
    IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    eAll, eCommunications, eConsole, eMultimedia,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::{GUID, HSTRING, PCWSTR, implement};

use crate::com::ComApartment;

/// One endpoint's volume and mute.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// 0.0 to 1.0, as Windows' volume slider.
    pub volume: f32,
    pub muted: bool,
}

/// `IPolicyConfig` (Windows 7 and later), in a module of its own: its
/// methods keep Windows' names. Only the last two are called; the others
/// keep the vtable's order.
#[allow(non_snake_case)]
mod policy_config {
    use std::ffi::c_void;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::ERole;
    use windows::core::{HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self, id: PCWSTR, format: *mut *mut c_void) -> HRESULT;
        fn GetDeviceFormat(&self, id: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
        fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
        fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
        fn GetProcessingPeriod(
            &self,
            id: PCWSTR,
            default: i32,
            a: *mut i64,
            b: *mut i64,
        ) -> HRESULT;
        fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
        fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn GetPropertyValue(
            &self,
            id: PCWSTR,
            key: *const PROPERTYKEY,
            value: *mut c_void,
        ) -> HRESULT;
        fn SetPropertyValue(
            &self,
            id: PCWSTR,
            key: *const PROPERTYKEY,
            value: *mut c_void,
        ) -> HRESULT;
        pub fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
        pub fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
    }
}
use policy_config::IPolicyConfig;

/// `CPolicyConfigClient`.
const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

fn words(what: &str, e: &windows::core::Error) -> String {
    format!("Windows didn't {what} ({}).", e.message().trim())
}

fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    // SAFETY: COM is initialized on this thread by the caller (each public
    // function holds a `ComApartment` for its whole body).
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn endpoint_volume(native_id: &str) -> windows::core::Result<IAudioEndpointVolume> {
    let id = HSTRING::from(native_id);
    // SAFETY: COM is initialized (caller); `id` outlives the call.
    let device = unsafe { enumerator()?.GetDevice(&id) }?;
    // SAFETY: activating the documented endpoint volume interface.
    unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }
}

/// An endpoint's volume and mute; `None` when it has none (or is gone).
pub fn settings(native_id: &str) -> Option<Settings> {
    let _com = ComApartment::enter().ok()?;
    let v = endpoint_volume(native_id).ok()?;
    // SAFETY: `v` is a valid interface for these calls.
    let volume = unsafe { v.GetMasterVolumeLevelScalar() }.ok()?;
    // SAFETY: as above.
    let muted = unsafe { v.GetMute() }.ok()?.as_bool();
    Some(Settings { volume, muted })
}

pub fn set_volume(native_id: &str, volume: f32) -> Result<(), String> {
    let _com = ComApartment::enter().map_err(|e| words("start COM", &e))?;
    let v = endpoint_volume(native_id).map_err(|e| words("open the device's volume", &e))?;
    // SAFETY: `v` is valid; a null event context is allowed.
    unsafe { v.SetMasterVolumeLevelScalar(volume.clamp(0.0, 1.0), std::ptr::null()) }
        .map_err(|e| words("change the volume", &e))
}

pub fn set_muted(native_id: &str, muted: bool) -> Result<(), String> {
    let _com = ComApartment::enter().map_err(|e| words("start COM", &e))?;
    let v = endpoint_volume(native_id).map_err(|e| words("open the device's volume", &e))?;
    // SAFETY: `v` is valid; a null event context is allowed.
    unsafe { v.SetMute(muted, std::ptr::null()) }
        .map_err(|e| words(if muted { "mute it" } else { "unmute it" }, &e))
}

fn policy() -> Result<IPolicyConfig, String> {
    // SAFETY: COM is initialized (caller); the class is Windows' own
    // policy configuration client.
    unsafe { CoCreateInstance::<_, IPolicyConfig>(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL) }
        .map_err(|e| words("open its sound settings", &e))
}

/// Makes an endpoint the default device (the console and multimedia roles,
/// as "Set as Default Device"), or the default communications device.
pub fn set_default(native_id: &str, communications: bool) -> Result<(), String> {
    let _com = ComApartment::enter().map_err(|e| words("start COM", &e))?;
    let p = policy()?;
    let id = HSTRING::from(native_id);
    let roles: &[ERole] = if communications {
        &[eCommunications]
    } else {
        &[eConsole, eMultimedia]
    };
    for role in roles {
        // SAFETY: `p` is valid; `id` outlives the call.
        unsafe { p.SetDefaultEndpoint(PCWSTR(id.as_ptr()), *role) }
            .ok()
            .map_err(|e| words("make it the default", &e))?;
    }
    Ok(())
}

/// Enables or disables an endpoint (as the Sound control panel's
/// "Enable" and "Disable").
pub fn set_enabled(native_id: &str, enabled: bool) -> Result<(), String> {
    let _com = ComApartment::enter().map_err(|e| words("start COM", &e))?;
    let p = policy()?;
    let id = HSTRING::from(native_id);
    // SAFETY: `p` is valid; `id` outlives the call.
    unsafe { p.SetEndpointVisibility(PCWSTR(id.as_ptr()), i32::from(enabled)) }
        .ok()
        .map_err(|e| words(if enabled { "enable it" } else { "disable it" }, &e))
}

type Waker = Arc<dyn Fn() + Send + Sync>;

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeNotice(Waker);

impl IAudioEndpointVolumeCallback_Impl for VolumeNotice_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        (self.0)();
        Ok(())
    }
}

/// Device changes: the waker, and the watch thread re-registers volumes.
#[implement(IMMNotificationClient)]
struct DeviceNotice {
    waker: Waker,
    rebuild: mpsc::Sender<bool>,
}

impl DeviceNotice_Impl {
    fn devices_changed(&self) {
        let _ = self.rebuild.send(true);
        (self.waker)();
    }
}

impl IMMNotificationClient_Impl for DeviceNotice_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        self.devices_changed();
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.devices_changed();
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.devices_changed();
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        _: EDataFlow,
        _: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        (self.waker)();
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Stops the watch when dropped.
#[derive(Debug)]
pub struct SettingsWatch {
    stop: Option<mpsc::Sender<bool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SettingsWatch {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(false);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Calls `waker` whenever an endpoint's volume or mute changes, a default
/// changes, or an endpoint is added, removed, enabled or disabled, however
/// it was changed (the taskbar, keys, Sound settings, other apps, or
/// AudioNet).
pub fn watch(waker: Waker) -> Result<SettingsWatch, String> {
    let (tx, rx) = mpsc::channel::<bool>();
    let rebuild = tx.clone();
    let thread = std::thread::Builder::new()
        .name("audionet-sound-settings".into())
        .spawn(move || {
            let Ok(_com) = ComApartment::enter() else {
                return;
            };
            let Ok(en) = enumerator() else { return };
            let client: IMMNotificationClient = DeviceNotice {
                waker: Arc::clone(&waker),
                rebuild,
            }
            .into();
            // SAFETY: `client` stays alive (held here) until unregistered.
            if unsafe { en.RegisterEndpointNotificationCallback(&client) }.is_err() {
                return;
            }
            let callback: IAudioEndpointVolumeCallback = VolumeNotice(Arc::clone(&waker)).into();
            let mut registered: Vec<IAudioEndpointVolume> = Vec::new();
            loop {
                for v in registered.drain(..) {
                    // SAFETY: registered below with this same callback.
                    let _ = unsafe { v.UnregisterControlChangeNotify(&callback) };
                }
                // SAFETY: `en` is valid; documented arguments.
                if let Ok(list) = unsafe { en.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE) } {
                    // SAFETY: `list` is valid.
                    let count = unsafe { list.GetCount() }.unwrap_or(0);
                    for i in 0..count {
                        // SAFETY: `i` is below the count; a vanished device
                        // just fails.
                        let Ok(d) = (unsafe { list.Item(i) }) else {
                            continue;
                        };
                        // SAFETY: activating the documented interface.
                        let Ok(v) =
                            (unsafe { d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) })
                        else {
                            continue;
                        };
                        // SAFETY: `callback` outlives the registration.
                        if unsafe { v.RegisterControlChangeNotify(&callback) }.is_ok() {
                            registered.push(v);
                        }
                    }
                }
                // Until devices change (rebuild) or the watch is dropped.
                // (A burst of changes is one rebuild; a stop is never lost.)
                let mut stop = !matches!(rx.recv(), Ok(true));
                while let Ok(m) = rx.try_recv() {
                    stop |= !m;
                }
                if stop {
                    break;
                }
            }
            for v in registered.drain(..) {
                // SAFETY: as above.
                let _ = unsafe { v.UnregisterControlChangeNotify(&callback) };
            }
            // SAFETY: the client registered above, on this thread.
            let _ = unsafe { en.UnregisterEndpointNotificationCallback(&client) };
        })
        .map_err(|e| format!("starting the sound settings watch: {e}"))?;
    Ok(SettingsWatch {
        stop: Some(tx),
        thread: Some(thread),
    })
}
