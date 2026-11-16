//! Linux device control against a real PulseAudio (run by hand: start one,
//! for example in a container with two null sinks "speakers" and
//! "headphones", then `PULSE_SERVER=unix:/path/native AN_PULSE_CONTAINER=name
//! cargo test -p audionet-node --test pulse_settings -- --ignored`). Changes
//! made by AudioNet and by another program (`pactl` in the container) are
//! read back and noticed.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

use audionet_node::audio::NodeAudio;
use audionet_node::cpal_audio::CpalNodeAudio;
use audionet_protocol::signal::{ControlAction, DestinationInfo, EndpointControl};

fn output(audio: &CpalNodeAudio, name: &str) -> (DestinationInfo, EndpointControl) {
    let (_, d) = audio.endpoints().expect("endpoints");
    let o = d
        .into_iter()
        .find(|o| o.id == format!("output:pulseaudio:{name}"))
        .unwrap_or_else(|| panic!("{name} listed"));
    let c = o.control.clone().expect("its settings");
    (o, c)
}

fn noticed(n: &AtomicUsize, before: usize) -> bool {
    let start = Instant::now();
    while n.load(SeqCst) == before && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    n.load(SeqCst) > before
}

#[test]
#[ignore = "needs a PulseAudio server with sinks \"speakers\" and \"headphones\""]
fn outputs_are_read_changed_and_noticed() {
    let audio = CpalNodeAudio;
    let (o, c) = output(&audio, "speakers");
    println!("{}: {c:?}", o.name);
    assert!(c.can_set_volume && c.can_mute && c.can_set_default);
    assert!(!c.can_enable, "a null sink has no card to turn off");

    let notices = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&notices);
    audio.watch_changes(Arc::new(move || {
        n.fetch_add(1, SeqCst);
    }));
    std::thread::sleep(Duration::from_millis(500));

    // Asked by the account.
    let before = notices.load(SeqCst);
    audio
        .control(&ControlAction::SetVolume {
            target: "output:pulseaudio:speakers".into(),
            volume: 0.4,
        })
        .unwrap();
    assert!(noticed(&notices, before), "the volume change was noticed");
    assert!((output(&audio, "speakers").1.volume.unwrap() - 0.4).abs() < 0.01);
    audio
        .control(&ControlAction::SetMuted {
            target: "output:pulseaudio:speakers".into(),
            muted: true,
        })
        .unwrap();
    assert_eq!(output(&audio, "speakers").1.muted, Some(true));
    // The sound playing on it: its output's mute.
    audio
        .control(&ControlAction::SetMuted {
            target: "input:pulseaudio:speakers.monitor".into(),
            muted: false,
        })
        .unwrap();
    assert_eq!(output(&audio, "speakers").1.muted, Some(false));
    audio
        .control(&ControlAction::SetDefault {
            target: "output:pulseaudio:headphones".into(),
            communications: false,
        })
        .unwrap();
    assert!(output(&audio, "headphones").0.is_default);
    assert!(!output(&audio, "speakers").0.is_default);
    let e = audio
        .control(&ControlAction::SetEnabled {
            target: "output:pulseaudio:speakers".into(),
            enabled: false,
        })
        .unwrap_err();
    assert!(e.contains("no sound card"), "{e}");
    assert!(
        audio
            .control(&ControlAction::SetDefault {
                target: "output:pulseaudio:speakers".into(),
                communications: true
            })
            .is_err()
    );

    // Changed by another program.
    if let Ok(container) = std::env::var("AN_PULSE_CONTAINER") {
        let before = notices.load(SeqCst);
        let started = Instant::now();
        let ok = std::process::Command::new("docker")
            .args([
                "exec",
                "-u",
                "1000",
                &container,
                "pactl",
                "--server",
                "unix:/run/pa/native",
                "set-sink-volume",
                "headphones",
                "25%",
            ])
            .status()
            .unwrap()
            .success();
        assert!(ok, "pactl ran");
        assert!(noticed(&notices, before), "pactl's change was noticed");
        println!("noticed pactl's change after {:?}", started.elapsed());
        assert!((output(&audio, "headphones").1.volume.unwrap() - 0.25).abs() < 0.01);
    }
}
