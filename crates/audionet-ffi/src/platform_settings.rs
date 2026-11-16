//! A phone's own volumes (Android's media, call, ring, alarm and
//! notification volumes), which the app reads and changes through the
//! system (Kotlin: `AudioManager`) for the account's device control: the
//! app gives the engine a [`PlatformSoundSettings`], and calls
//! [`sound_settings_changed`] whenever the system says a volume changed
//! (however it was changed: the volume keys, the system panel, another
//! app, or AudioNet), so the account sees it at once.

use std::sync::{Arc, Mutex};

use audionet_node::audio::ChangeWaker;
use audionet_protocol::signal::{ControlAction, DeviceVolume as WireVolume};

use crate::DeviceVolume;

/// Implemented by the app. Called on engine threads (never real-time ones).
#[uniffi::export(with_foreign)]
pub trait PlatformSoundSettings: Send + Sync {
    /// The phone's volumes now (ids such as "stream:music", stable while
    /// the app runs; `volume` 0.0 to 1.0).
    fn volumes(&self) -> Vec<DeviceVolume>;
    /// Sets one (0.0 to 1.0); `None` when done, else why not, in words.
    fn set_volume(&self, id: String, volume: f32) -> Option<String>;
    /// Mutes or unmutes one; `None` when done, else why not.
    fn set_muted(&self, id: String, muted: bool) -> Option<String>;
}

static SETTINGS: Mutex<Option<Arc<dyn PlatformSoundSettings>>> = Mutex::new(None);
static WAKER: Mutex<Option<ChangeWaker>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Gives the engine the phone's volumes (`None` takes them away). Call it
/// before starting the engine.
#[uniffi::export]
pub fn set_platform_sound_settings(settings: Option<Arc<dyn PlatformSoundSettings>>) {
    *lock(&SETTINGS) = settings;
}

/// The app tells the engine a volume changed (from any thread).
#[uniffi::export]
pub fn sound_settings_changed() {
    let waker = lock(&WAKER).clone();
    if let Some(w) = waker {
        w();
    }
}

pub(crate) fn watch(waker: ChangeWaker) {
    *lock(&WAKER) = Some(waker);
}

pub(crate) fn volumes() -> Vec<WireVolume> {
    let s = lock(&SETTINGS).clone();
    s.map(|s| {
        s.volumes()
            .into_iter()
            .map(|v| WireVolume {
                id: v.id,
                name: v.name,
                volume: v.volume.clamp(0.0, 1.0),
                muted: v.muted,
                can_mute: v.can_mute,
            })
            .collect()
    })
    .unwrap_or_default()
}

/// A change to one of the phone's volumes, if `action` names one (`None`
/// when it is about something else).
pub(crate) fn control(action: &ControlAction) -> Option<Result<(), String>> {
    let s = lock(&SETTINGS).clone()?;
    let target = action.target();
    if !s.volumes().iter().any(|v| v.id == target) {
        return None;
    }
    let error = match action {
        ControlAction::SetVolume { volume, .. } => {
            s.set_volume(target.to_owned(), volume.clamp(0.0, 1.0))
        }
        ControlAction::SetMuted { muted, .. } => s.set_muted(target.to_owned(), *muted),
        ControlAction::SetDefault { .. } => Some("That volume can't be made a default.".to_owned()),
        ControlAction::SetEnabled { .. } => Some("That volume can't be turned off.".to_owned()),
    };
    Some(error.map_or(Ok(()), Err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use audionet_node::audio::NodeAudio;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    /// A stand-in phone with a media volume and an alarm without mute.
    struct Phone(Mutex<(f32, bool)>);

    impl PlatformSoundSettings for Phone {
        fn volumes(&self) -> Vec<DeviceVolume> {
            let (v, m) = *self.0.lock().unwrap();
            vec![
                DeviceVolume {
                    id: "stream:music".into(),
                    name: "Media".into(),
                    volume: v,
                    muted: Some(m),
                    can_mute: true,
                },
                DeviceVolume {
                    id: "stream:alarm".into(),
                    name: "Alarm".into(),
                    volume: 0.5,
                    muted: None,
                    can_mute: false,
                },
            ]
        }
        fn set_volume(&self, id: String, volume: f32) -> Option<String> {
            (id == "stream:music").then(|| self.0.lock().unwrap().0 = volume);
            None
        }
        fn set_muted(&self, id: String, muted: bool) -> Option<String> {
            if id == "stream:alarm" {
                return Some("The alarm volume can't be muted.".into());
            }
            self.0.lock().unwrap().1 = muted;
            None
        }
    }

    #[test]
    fn the_apps_volumes_are_listed_changed_and_noticed() {
        let phone = Arc::new(Phone(Mutex::new((0.3, false))));
        set_platform_sound_settings(Some(phone.clone()));
        let audio = crate::tap::AppAudio;
        let (_, volumes) = audio.extra_endpoints().unwrap();
        assert!(
            volumes
                .iter()
                .any(|v| v.id == "stream:music" && (v.volume - 0.3).abs() < 1e-6)
        );
        audio
            .control(&ControlAction::SetVolume {
                target: "stream:music".into(),
                volume: 0.8,
            })
            .unwrap();
        audio
            .control(&ControlAction::SetMuted {
                target: "stream:music".into(),
                muted: true,
            })
            .unwrap();
        assert_eq!(*phone.0.lock().unwrap(), (0.8, true));
        let e = audio
            .control(&ControlAction::SetMuted {
                target: "stream:alarm".into(),
                muted: true,
            })
            .unwrap_err();
        assert!(e.contains("can't be muted"));
        // Not one of the phone's: the platform's own devices answer.
        assert!(
            control(&ControlAction::SetVolume {
                target: "output:x".into(),
                volume: 0.1
            })
            .is_none()
        );
        // The app's notice reaches the engine.
        let n = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&n);
        audio.watch_changes(Arc::new(move || {
            c.fetch_add(1, SeqCst);
        }));
        sound_settings_changed();
        assert_eq!(n.load(SeqCst), 1);
        set_platform_sound_settings(None);
        assert!(
            audio
                .extra_endpoints()
                .unwrap()
                .1
                .iter()
                .all(|v| !v.id.starts_with("stream:"))
        );
    }
}
