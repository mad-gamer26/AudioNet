//! A session survives its audio device disappearing and coming back.
//!
//! Drives a real session thread (answering a real str0m offer) with a fake
//! audio backend whose device fails, stays unavailable for a while, then
//! returns. The loss handling runs whether or not ICE has connected, so no
//! network peer is needed.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audionet_audio::render::RenderSource;
use audionet_audio::ring::{RingProducer, audio_ring};
use audionet_node::audio::{NodeAudio, OpenCapture, StreamGuard};
use audionet_node::session::{self, SessionEvent};
use audionet_protocol::SessionId;
use audionet_protocol::signal::{DestinationInfo, SessionMedia, SessionState, SourceInfo};
use str0m::RtcConfig;
use str0m::media::{Direction, MediaKind};

/// A device that can be unplugged (`present = false`) and plugged back in.
#[derive(Default)]
struct FakeDevice {
    present: AtomicBool,
    /// Set to make the currently open stream report a failure.
    fail_open_stream: Arc<AtomicBool>,
    opens: AtomicUsize,
    /// Keeps capture producers alive so the rings stay connected.
    producers: Mutex<Vec<RingProducer>>,
}

struct FakeGuard(Arc<AtomicBool>);

impl StreamGuard for FakeGuard {
    fn failure(&mut self) -> Option<String> {
        self.0
            .load(SeqCst)
            .then(|| "the device was disconnected".to_owned())
    }
}

impl FakeDevice {
    fn open(&self) -> Result<Box<dyn StreamGuard>, String> {
        if !self.present.load(SeqCst) {
            return Err("no such device".into());
        }
        self.opens.fetch_add(1, SeqCst);
        self.fail_open_stream.store(false, SeqCst);
        Ok(Box::new(FakeGuard(Arc::clone(&self.fail_open_stream))))
    }

    fn unplug(&self) {
        self.present.store(false, SeqCst);
        self.fail_open_stream.store(true, SeqCst);
    }
}

impl NodeAudio for FakeDevice {
    fn endpoints(&self) -> Result<(Vec<SourceInfo>, Vec<DestinationInfo>), String> {
        Ok((Vec::new(), Vec::new()))
    }

    fn open_source(&self, _source_id: &str) -> Result<OpenCapture, String> {
        let guard = self.open()?;
        let (producer, consumer) = audio_ring(48_000, 2);
        self.producers.lock().unwrap().push(producer);
        Ok(OpenCapture {
            consumer,
            sample_rate: 48_000,
            guard,
            description: "fake input".into(),
        })
    }

    fn open_destination(
        &self,
        _destination_id: &str,
        _source: Box<dyn RenderSource>,
    ) -> Result<(Box<dyn StreamGuard>, String), String> {
        Ok((self.open()?, "fake output".into()))
    }
}

fn offer(direction: Direction) -> String {
    let mut peer = RtcConfig::new()
        .clear_codecs()
        .enable_opus(true)
        .build(Instant::now());
    let mut change = peer.sdp_api();
    change.add_media(MediaKind::Audio, direction, None, None, None);
    let (offer, _pending) = change.apply().expect("an offer");
    offer.to_sdp_string()
}

/// Runs a session, unplugs the device after 300 ms, plugs it back in after
/// 2.5 s, and returns every status seen plus how often the device opened.
fn unplug_and_return(
    media: SessionMedia,
    offer_dir: Direction,
) -> (Vec<(SessionState, String)>, usize) {
    let device = Arc::new(FakeDevice::default());
    device.present.store(true, SeqCst);
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&statuses);
    let mut handle = session::start(
        SessionId::new("device-loss-test").unwrap(),
        session::LocalMedia::answering(&media),
        session::Negotiation::Answer {
            offer_sdp: offer(offer_dir),
        },
        session::SessionNetwork::default(),
        Arc::clone(&device) as Arc<dyn NodeAudio>,
        Arc::new(move |event| {
            if let SessionEvent::Status { state, detail, .. } = event {
                sink.lock().unwrap().push((state, detail));
            }
        }),
        None,
    );
    std::thread::sleep(Duration::from_millis(300));
    device.unplug();
    std::thread::sleep(Duration::from_millis(2200));
    device.present.store(true, SeqCst);
    std::thread::sleep(Duration::from_millis(1500));
    handle.stop();
    let seen = statuses.lock().unwrap().clone();
    (seen, device.opens.load(SeqCst))
}

fn assert_waited_then_resumed(seen: &[(SessionState, String)], opens: usize, stopped: &str) {
    let waiting = seen
        .iter()
        .position(|(state, d)| *state == SessionState::Starting && d.starts_with(stopped))
        .unwrap_or_else(|| panic!("no waiting status in {seen:#?}"));
    assert!(
        seen[waiting]
            .1
            .contains("Waiting up to 30 seconds for the device to come back."),
        "{seen:#?}"
    );
    let back = seen
        .iter()
        .position(|(state, d)| {
            // Not yet connected (no network peer), so the state stays Starting.
            *state == SessionState::Starting && d.starts_with("The audio device is back.")
        })
        .unwrap_or_else(|| panic!("no recovery status in {seen:#?}"));
    assert!(back > waiting);
    assert!(
        !seen.iter().any(|(state, _)| *state == SessionState::Failed),
        "{seen:#?}"
    );
    assert_eq!(opens, 2, "opened once at start and once on return");
}

#[test]
fn listen_session_waits_for_capture_device_to_return() {
    let (seen, opens) = unplug_and_return(
        SessionMedia::Listen {
            source_id: "loopback:fake".into(),
        },
        Direction::RecvOnly,
    );
    assert_waited_then_resumed(
        &seen,
        opens,
        "Audio capture stopped: the device was disconnected.",
    );
}

#[test]
fn speak_session_waits_for_output_device_to_return() {
    let (seen, opens) = unplug_and_return(
        SessionMedia::Speak {
            destination_id: "output:fake".into(),
        },
        Direction::SendOnly,
    );
    assert_waited_then_resumed(
        &seen,
        opens,
        "Audio playback stopped: the device was disconnected.",
    );
}
