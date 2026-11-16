//! On real hardware (run by hand on a Mac: `cargo test -p audionet-coreaudio
//! --test settings_hw -- --ignored --nocapture`): reads the default output's
//! settings, nudges its volume by 2% and back, and checks the change is read
//! back and noticed. Mute and the default device are left alone.

#[cfg(target_os = "macos")]
#[test]
#[ignore = "changes the Mac's volume slightly (and restores it)"]
fn the_default_output_volume_is_read_changed_and_noticed() {
    use audionet_coreaudio::settings;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::{Duration, Instant};

    let uid = settings::default_device(false).expect("a default output");
    let before = settings::settings(&uid, false).expect("its settings");
    println!("Default output {uid}: {before:?}");
    let input = settings::default_device(true);
    if let Some(i) = &input {
        println!("Default input {i}: {:?}", settings::settings(i, true));
    }
    let Some(v) = before.volume.filter(|_| before.can_set_volume) else {
        println!("Its volume can't be set: nothing more to check.");
        return;
    };
    let notices = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&notices);
    settings::watch(Arc::new(move || {
        n.fetch_add(1, SeqCst);
    }));
    let nudged = if v > 0.5 { v - 0.02 } else { v + 0.02 };
    settings::set_volume(&uid, false, nudged).expect("volume set");
    let after = settings::settings(&uid, false).unwrap().volume.unwrap();
    let start = Instant::now();
    while notices.load(SeqCst) == 0 && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let noticed = notices.load(SeqCst);
    settings::set_volume(&uid, false, v).expect("volume restored");
    println!(
        "Nudged to {after} (asked {nudged}); notices: {noticed} after {:?}",
        start.elapsed()
    );
    assert!(
        (after - nudged).abs() < 0.02,
        "read back {after}, asked {nudged}"
    );
    assert!(noticed > 0, "the change was noticed");
    let restored = settings::settings(&uid, false).unwrap().volume.unwrap();
    assert!(
        (restored - v).abs() < 0.02,
        "restored to {restored}, was {v}"
    );
}
