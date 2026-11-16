//! On real hardware (run by hand on Windows: `cargo test -p audionet-wasapi
//! --test settings_hw -- --ignored --nocapture`): every active endpoint's
//! settings are read; the default output's volume is nudged by 2% and back,
//! read back and noticed. Defaults and enabled devices are left alone.

#[cfg(windows)]
#[test]
#[ignore = "changes the PC's volume slightly (and restores it)"]
fn the_default_output_volume_is_read_changed_and_noticed() {
    use audionet_audio::{EndpointEnumerator, EnumerationOptions};
    use audionet_protocol::{DefaultRole, Direction, EndpointState};
    use audionet_wasapi::{WasapiEnumerator, settings};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::{Duration, Instant};

    let inv = WasapiEnumerator
        .enumerate(EnumerationOptions::default())
        .unwrap();
    for e in inv.endpoints() {
        let s = if e.state == EndpointState::Active {
            settings::settings(e.id.native_id())
        } else {
            None
        };
        println!(
            "{:?} {:?} {:?} {}: {s:?}",
            e.direction, e.state, e.default_roles, e.name
        );
    }
    let out = inv
        .endpoints()
        .iter()
        .find(|e| {
            e.direction == Direction::Output && e.default_roles.contains(&DefaultRole::Console)
        })
        .expect("a default output");
    let id = out.id.native_id();
    let v = settings::settings(id).expect("its volume").volume;

    let notices = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&notices);
    let _watch = settings::watch(Arc::new(move || {
        n.fetch_add(1, SeqCst);
    }))
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let nudged = if v > 0.5 { v - 0.02 } else { v + 0.02 };
    settings::set_volume(id, nudged).unwrap();
    let after = settings::settings(id).unwrap().volume;
    let start = Instant::now();
    while notices.load(SeqCst) == 0 && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let noticed = notices.load(SeqCst);
    settings::set_volume(id, v).unwrap();
    println!(
        "{}: {v} nudged to {after}; notices {noticed} after {:?}",
        out.name,
        start.elapsed()
    );
    assert!((after - nudged).abs() < 0.01);
    assert!(noticed > 0, "the change was noticed");
    assert!(
        (settings::settings(id).unwrap().volume - v).abs() < 0.01,
        "restored"
    );
}
