//! Audio device change notifications (`IMMNotificationClient`).
//!
//! Threading: [`watch_devices`] starts one ordinary thread that enters the
//! multithreaded apartment, registers the notification client and then
//! only waits to be told to stop, so the apartment and the registration
//! never depend on the caller's thread. Windows calls the client on its own
//! MTA threads; the callback given to `watch_devices` runs there and must
//! return quickly (post a window message, set a flag). These notifications
//! are control path only; nothing here touches audio callbacks.

use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::{PCWSTR, implement};

use crate::com::ComApartment;

type Changed = Box<dyn Fn() + Send + Sync>;

#[implement(IMMNotificationClient)]
struct Client {
    changed: Changed,
}

impl IMMNotificationClient_Impl for Client_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        (self.changed)();
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        (self.changed)();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        (self.changed)();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        _: EDataFlow,
        _: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        (self.changed)();
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, key: &PROPERTYKEY) -> windows::core::Result<()> {
        // Only a renamed device changes what is listed; drivers report
        // many other property changes.
        if *key == PKEY_Device_FriendlyName {
            (self.changed)();
        }
        Ok(())
    }
}

/// Watches for audio devices being added, removed, enabled, disabled,
/// renamed, or a new default; stops when dropped.
#[derive(Debug)]
pub struct DeviceWatch {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for DeviceWatch {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Calls `changed` (on a Windows thread, quickly) whenever the audio
/// devices change. Several calls can come for one change; debounce them.
pub fn watch_devices(changed: impl Fn() + Send + Sync + 'static) -> Result<DeviceWatch, String> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let changed: Changed = Box::new(changed);
    let thread = std::thread::Builder::new()
        .name("audionet-device-watch".into())
        .spawn(move || {
            let _com = match ComApartment::enter() {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("starting COM: {e}")));
                    return;
                }
            };
            let registered = (move || -> windows::core::Result<_> {
                // SAFETY: COM is initialized on this thread (`_com`, alive
                // until the end of this closure's caller); the class id
                // and interface are the MMDevice API's.
                let enumerator: IMMDeviceEnumerator =
                    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
                let client: IMMNotificationClient = Client { changed }.into();
                // SAFETY: `client` is a valid COM object that stays alive
                // (held below) until it is unregistered.
                unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }?;
                Ok((enumerator, client))
            })();
            match registered {
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("watching audio devices: {e}")));
                }
                Ok((enumerator, client)) => {
                    let _ = ready_tx.send(Ok(()));
                    // Until the watch is dropped (its sender closes).
                    let _ = stop_rx.recv();
                    // SAFETY: the same client registered above, unregistered
                    // once, on the thread and apartment that registered it;
                    // both are released before `_com` is dropped.
                    unsafe {
                        let _ = enumerator.UnregisterEndpointNotificationCallback(&client);
                    }
                }
            }
        })
        .map_err(|e| format!("starting the device watch thread: {e}"))?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(DeviceWatch {
            stop: Some(stop_tx),
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err("the device watch thread ended".into())
        }
    }
}
