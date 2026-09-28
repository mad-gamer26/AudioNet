//! Two native devices, end to end through a real server: each signs in
//! with the account password, one drives the other as a remote, and audio
//! really flows (through the real Opus, WebRTC and playout path) between
//! simulated sound hardware on both devices, in both directions.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audionet_audio::render::RenderSource;
use audionet_audio::ring::audio_ring;
use audionet_node::agent::{Agent, AppEvent, Command, Control};
use audionet_node::audio::{NodeAudio, OpenCapture, StreamGuard};
use audionet_node::config::NodeConfig;
use audionet_node::session::LocalMedia;
use audionet_protocol::signal::{
    DestinationInfo, SessionMedia, SessionState, SourceInfo, SourceType,
};
use audionet_protocol::{NodeId, Platform, SessionId};
use audionet_server::api::{AppState, router};
use audionet_server::config::Config;
use audionet_server::{auth, db};
use tokio::sync::mpsc;

const RATE: u32 = 48_000;
const TONE_HZ: f64 = 997.0;

/// Simulated sound hardware: a microphone that hears a steady tone, and
/// speakers that record what they are asked to play.
#[derive(Default)]
struct FakeAudio {
    played: Arc<Mutex<Vec<f32>>>,
    timing: Arc<Timing>,
}

/// How well the simulated sound cards kept real time (for a failure's
/// message: a slow test machine shows here, a network or playout problem
/// does not).
#[derive(Default)]
struct Timing {
    /// Longest single render call, in microseconds.
    render_max_us: std::sync::atomic::AtomicU64,
    /// Most 10 ms blocks the speakers or microphone were behind at once.
    speaker_lag_blocks: std::sync::atomic::AtomicU64,
    mic_lag_blocks: std::sync::atomic::AtomicU64,
}

impl FakeAudio {
    fn timing(&self) -> String {
        let t = &self.timing;
        format!(
            "longest render {} us; most blocks behind: speakers {}, microphone {}",
            t.render_max_us.load(Relaxed),
            t.speaker_lag_blocks.load(Relaxed),
            t.mic_lag_blocks.load(Relaxed)
        )
    }
}

/// The newest receive diagnostics among the events waiting (other events
/// are left out): what the receiving side measured.
fn latest_diagnostics(rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> String {
    let mut last = String::from("no diagnostics yet");
    while let Ok(e) = rx.try_recv() {
        if let AppEvent::Diagnostics { text, .. } = e {
            last = text;
        }
    }
    last
}

struct Guard(Arc<AtomicBool>);

impl StreamGuard for Guard {
    fn failure(&mut self) -> Option<String> {
        None
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Relaxed);
    }
}

impl NodeAudio for FakeAudio {
    fn endpoints(&self) -> Result<(Vec<SourceInfo>, Vec<DestinationInfo>), String> {
        Ok((
            vec![SourceInfo {
                id: "input:mic".into(),
                name: "Test microphone".into(),
                source_type: SourceType::Input,
                is_default: true,
            }],
            vec![DestinationInfo {
                id: "output:speakers".into(),
                name: "Test speakers".into(),
                is_default: true,
            }],
        ))
    }

    fn open_source(&self, _id: &str) -> Result<OpenCapture, String> {
        let (mut producer, consumer) = audio_ring(RATE as usize, 2);
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&stop);
        let timing = Arc::clone(&self.timing);
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut written = 0u64;
            let mut block = vec![0f32; 480 * 2];
            while !done.load(Relaxed) {
                // Real-time pacing, like a sound card.
                let due = (start.elapsed().as_secs_f64() * f64::from(RATE)) as u64;
                timing
                    .mic_lag_blocks
                    .fetch_max(due.saturating_sub(written) / 480, Relaxed);
                while written + 480 <= due {
                    for (i, frame) in block.chunks_mut(2).enumerate() {
                        let t = (written + i as u64) as f64 / f64::from(RATE);
                        let v = (0.25 * (2.0 * std::f64::consts::PI * TONE_HZ * t).sin()) as f32;
                        frame[0] = v;
                        frame[1] = v;
                    }
                    let _ = producer.write(&block);
                    written += 480;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Ok(OpenCapture {
            consumer,
            sample_rate: RATE,
            guard: Box::new(Guard(stop)),
            description: "Test microphone".into(),
        })
    }

    fn open_destination(
        &self,
        _id: &str,
        mut source: Box<dyn RenderSource>,
    ) -> Result<(Box<dyn StreamGuard>, String), String> {
        source.prepare(RATE, 2, 480)?;
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&stop);
        let played = Arc::clone(&self.played);
        let timing = Arc::clone(&self.timing);
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut rendered = 0u64;
            let mut buf = vec![0f32; 480 * 2];
            while !done.load(Relaxed) {
                let due = (start.elapsed().as_secs_f64() * f64::from(RATE)) as u64;
                timing
                    .speaker_lag_blocks
                    .fetch_max(due.saturating_sub(rendered) / 480, Relaxed);
                while rendered + 480 <= due {
                    let began = Instant::now();
                    source.render(&mut buf, audionet_audio::clock::now_ns());
                    timing
                        .render_max_us
                        .fetch_max(began.elapsed().as_micros() as u64, Relaxed);
                    let mut p = played.lock().unwrap();
                    p.extend(buf.iter().step_by(2));
                    let excess = p.len().saturating_sub(RATE as usize * 3);
                    p.drain(..excess);
                    rendered += 480;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Ok((Box::new(Guard(stop)), "Test speakers".into()))
    }
}

/// Half volume is about 12 dB quieter (the square curve), mute is silence,
/// and full volume comes back: measured on what `played` records.
async fn check_volume(
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    session: &SessionId,
    played: &Mutex<Vec<f32>>,
    full: f64,
    who: &str,
) {
    let set = |volume: f32, muted: bool| {
        commands
            .send(Command::SetVolume {
                session_id: session.clone(),
                volume,
                muted,
            })
            .unwrap();
    };
    set(0.5, false);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (half, _) = analyse(played);
    println!("{who} at half volume: {half:.1} dBFS (full {full:.1})");
    assert!(
        (full - half - 12.0).abs() < 2.0,
        "{who}: half volume is {half:.1} dBFS, full {full:.1}"
    );
    set(0.5, true);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (muted, _) = analyse(played);
    println!("{who} muted: {muted:.1} dBFS");
    assert!(muted < -60.0, "{who}: muted is {muted:.1} dBFS");
    set(1.0, false);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (back, _) = analyse(played);
    assert!(
        (back - full).abs() < 2.0,
        "{who}: back to full is {back:.1} dBFS, was {full:.1}"
    );
}

/// Level (dBFS) and frequency (zero crossings) of the last second played.
fn analyse(played: &Mutex<Vec<f32>>) -> (f64, f64) {
    let p = played.lock().unwrap();
    let last = &p[p.len().saturating_sub(RATE as usize)..];
    if last.len() < RATE as usize / 2 {
        return (f64::NEG_INFINITY, 0.0);
    }
    let rms = (last.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / last.len() as f64).sqrt();
    let crossings = last
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    let hz = crossings as f64 / 2.0 / (last.len() as f64 / f64::from(RATE));
    (20.0 * rms.max(1e-12).log10(), hz)
}

/// Runs of at least 5 ms of exact silence in the last second played.
fn silent_runs(played: &Mutex<Vec<f32>>) -> usize {
    let p = played.lock().unwrap();
    let last = &p[p.len().saturating_sub(RATE as usize)..];
    let (mut runs, mut len) = (0, 0);
    for v in last {
        if *v == 0.0 {
            len += 1;
            if len == RATE as usize / 200 {
                runs += 1;
            }
        } else {
            len = 0;
        }
    }
    runs
}

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = Config::parse(&format!("public_url = \"http://{addr}\"\n")).unwrap();
    config.validate().unwrap();
    let db = db::Db::open_in_memory().unwrap();
    let hash = auth::hash_password("correct horse battery").unwrap();
    db.with(|c| db::create_user(c, "alice", &hash)).unwrap();
    let state = Arc::new(AppState {
        config,
        db,
        hub: Default::default(),
        login_throttle: Default::default(),
        sign_up_limit: Default::default(),
    });
    let app = router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Signs a device in with the account password (as a native app does).
fn sign_in(base: &str, name: &str) -> NodeConfig {
    sign_in_as(base, "alice", "correct horse battery", name)
}

fn sign_in_as(base: &str, user: &str, password: &str, name: &str) -> NodeConfig {
    let base = base.to_owned();
    let name = name.to_owned();
    let (user, password) = (user.to_owned(), password.to_owned());
    std::thread::spawn(move || {
        let body: serde_json::Value = ureq::post(&format!("{base}/api/v1/nodes/sign-in"))
            .send_json(serde_json::json!({
                "username": user, "password": password,
                "name": name, "platform": "windows"
            }))
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        NodeConfig {
            server_url: base,
            node_id: body["node_id"].as_str().unwrap().into(),
            token: body["token"].as_str().unwrap().into(),
            name,
            username: body["username"].as_str().unwrap().into(),
        }
    })
    .join()
    .unwrap()
}

fn agent(config: NodeConfig, audio: Arc<FakeAudio>, control: Option<Control>) -> Agent {
    agent_with(config, audio, control, false)
}

fn agent_with(
    config: NodeConfig,
    audio: Arc<FakeAudio>,
    control: Option<Control>,
    relay_only: bool,
) -> Agent {
    Agent {
        config,
        audio,
        platform: Platform::Windows,
        software: "test".into(),
        status: Arc::new(|_| {}),
        thread_setup: None,
        active_sessions: Default::default(),
        control,
        relay_only,
        sharing: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    }
}

async fn next_event(
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    what: &str,
    mut pred: impl FnMut(&AppEvent) -> bool,
) -> AppEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let e = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap();
        // Nothing in these flows should draw an error from the server
        // (for example status sent for a session it does not know yet).
        if let AppEvent::ServerError { message } = &e {
            panic!("the server reported a problem while waiting for {what}: {message}");
        }
        if pred(&e) {
            return e;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_native_devices_stream_audio_both_ways() {
    let base = start_server().await;
    let pc_config = sign_in(&base, "Studio PC");
    let pc_id = NodeId::new(pc_config.node_id.clone()).unwrap();
    let phone_config = sign_in(&base, "iPhone");

    let pc_audio = Arc::new(FakeAudio::default());
    let phone_audio = Arc::new(FakeAudio::default());
    let (event_tx, mut events) = mpsc::unbounded_channel();
    let (control, commands) = Control::new(Arc::new(move |e| {
        let _ = event_tx.send(e);
    }));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    for (config, audio, control) in [
        (pc_config, Arc::clone(&pc_audio), None),
        (phone_config, Arc::clone(&phone_audio), Some(control)),
    ] {
        let mut stop = stop_rx.clone();
        tokio::spawn(agent(config, audio, control).run(async move {
            let _ = stop.changed().await;
        }));
    }

    // The phone app sees the PC come online with its microphone.
    next_event(&mut events, "the PC online", |e| match e {
        AppEvent::Devices(nodes) => nodes
            .iter()
            .any(|n| n.name == "Studio PC" && n.online && !n.sources.is_empty()),
        AppEvent::DeviceUpdate(n) => n.name == "Studio PC" && n.online && !n.sources.is_empty(),
        _ => false,
    })
    .await;

    // 1. The phone listens to the PC's microphone on its own speakers.
    let listen = SessionId::new("listen-1").unwrap();
    commands
        .send(Command::Start {
            session_id: listen.clone(),
            node_id: pc_id.clone(),
            remote: SessionMedia::Listen {
                source_id: "input:mic".into(),
            },
            local: LocalMedia::Receive {
                destination_id: "output:speakers".into(),
            },
        })
        .unwrap();
    next_event(&mut events, "listen connected", |e| {
        matches!(e, AppEvent::Session { session_id, state: SessionState::Active, .. } if *session_id == listen)
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (level, hz) = analyse(&phone_audio.played);
    println!("phone hears the PC: {level:.1} dBFS at {hz:.0} Hz");
    let why = format!(
        "phone: {}; PC {}; receiver: {}",
        phone_audio.timing(),
        pc_audio.timing(),
        latest_diagnostics(&mut events)
    );
    assert!(level > -20.0, "phone hears {level:.1} dBFS ({why})");
    assert!(
        (hz - TONE_HZ).abs() < 15.0,
        "phone hears {hz:.0} Hz ({why})"
    );
    // The listening stream's volume, on what the phone plays.
    let full = level;
    check_volume(&commands, &listen, &phone_audio.played, full, "phone").await;
    commands
        .send(Command::Stop {
            session_id: listen.clone(),
        })
        .unwrap();
    next_event(
        &mut events,
        "listen ended",
        |e| matches!(e, AppEvent::SessionEnded { session_id, .. } if *session_id == listen),
    )
    .await;

    // 2. The phone sends its microphone to the PC's speakers.
    let speak = SessionId::new("speak-1").unwrap();
    commands
        .send(Command::Start {
            session_id: speak.clone(),
            node_id: pc_id,
            remote: SessionMedia::Speak {
                destination_id: "output:speakers".into(),
            },
            local: LocalMedia::Send {
                source_id: "input:mic".into(),
            },
        })
        .unwrap();
    next_event(&mut events, "speak connected", |e| {
        matches!(e, AppEvent::Session { session_id, state: SessionState::Active, .. } if *session_id == speak)
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (level, hz) = analyse(&pc_audio.played);
    println!("PC plays the phone: {level:.1} dBFS at {hz:.0} Hz");
    assert!(level > -20.0, "PC plays {level:.1} dBFS");
    assert!((hz - TONE_HZ).abs() < 15.0, "PC plays {hz:.0} Hz");
    // The sending stream's volume, on what the phone sends.
    check_volume(&commands, &speak, &pc_audio.played, level, "PC").await;
    commands.send(Command::Stop { session_id: speak }).unwrap();
    let _ = stop_tx.send(true);
}

/// A device that does not share stays online: it receives audio (others
/// send to it, it listens to them) but sends none of its own, until it
/// shares; stopping sharing ends what it sends, on both sides.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_does_not_share_receives_but_does_not_send() {
    let base = start_server().await;
    let laptop_config = sign_in(&base, "Laptop");
    let laptop_id = NodeId::new(laptop_config.node_id.clone()).unwrap();
    let phone_config = sign_in(&base, "Phone");
    let phone_id = NodeId::new(phone_config.node_id.clone()).unwrap();

    let laptop_audio = Arc::new(FakeAudio::default());
    let phone_audio = Arc::new(FakeAudio::default());
    let laptop_sharing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ltx, mut laptop_events) = mpsc::unbounded_channel();
    let (laptop_control, laptop) = Control::new(Arc::new(move |e| {
        let _ = ltx.send(e);
    }));
    let (ptx, mut phone_events) = mpsc::unbounded_channel();
    let (phone_control, phone) = Control::new(Arc::new(move |e| {
        let _ = ptx.send(e);
    }));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut laptop_agent = agent(
        laptop_config,
        Arc::clone(&laptop_audio),
        Some(laptop_control),
    );
    laptop_agent.sharing = Arc::clone(&laptop_sharing);
    for a in [
        laptop_agent,
        agent(phone_config, Arc::clone(&phone_audio), Some(phone_control)),
    ] {
        let mut stop = stop_rx.clone();
        tokio::spawn(a.run(async move {
            let _ = stop.changed().await;
        }));
    }
    let seen = |sharing: bool| {
        move |e: &AppEvent| {
            let ok = |n: &audionet_protocol::signal::NodeSummary| {
                n.name == "Laptop" && n.online && n.sharing == sharing && !n.sources.is_empty()
            };
            match e {
                AppEvent::Devices(nodes) => nodes.iter().any(ok),
                AppEvent::DeviceUpdate(n) => ok(n),
                _ => false,
            }
        }
    };
    // 1. The phone sees the laptop online, not sharing.
    next_event(
        &mut phone_events,
        "the laptop online, not sharing",
        seen(false),
    )
    .await;
    // The laptop sees the phone too.
    next_event(&mut laptop_events, "the phone", |e| match e {
        AppEvent::Devices(nodes) => nodes.iter().any(|n| n.name == "Phone" && n.online),
        AppEvent::DeviceUpdate(n) => n.name == "Phone" && n.online,
        _ => false,
    })
    .await;

    let listen_to_laptop = |id: &str| Command::Start {
        session_id: SessionId::new(id).unwrap(),
        node_id: laptop_id.clone(),
        remote: SessionMedia::Listen {
            source_id: "input:mic".into(),
        },
        local: LocalMedia::Receive {
            destination_id: "output:speakers".into(),
        },
    };
    let ended = |id: &'static str| move |e: &AppEvent| matches!(e, AppEvent::SessionEnded { session_id, .. } if session_id.as_str() == id);
    let active = |id: &'static str| move |e: &AppEvent| matches!(e, AppEvent::Session { session_id, state: SessionState::Active, .. } if session_id.as_str() == id);

    // 2. Nobody may listen to it.
    phone.send(listen_to_laptop("refused")).unwrap();
    let e = next_event(&mut phone_events, "listening refused", ended("refused")).await;
    let AppEvent::SessionEnded { reason, .. } = e else {
        unreachable!()
    };
    assert!(reason.contains("\"Laptop\" is not sharing"), "{reason}");

    // 3. It may not send its own audio.
    laptop
        .send(Command::Start {
            session_id: SessionId::new("laptop-sends").unwrap(),
            node_id: phone_id.clone(),
            remote: SessionMedia::Speak {
                destination_id: "output:speakers".into(),
            },
            local: LocalMedia::Send {
                source_id: "input:mic".into(),
            },
        })
        .unwrap();
    let e = next_event(&mut laptop_events, "sending refused", ended("laptop-sends")).await;
    let AppEvent::SessionEnded { reason, .. } = e else {
        unreachable!()
    };
    assert!(reason.contains("not sharing"), "{reason}");

    // 4. It receives: the phone sends its microphone to the laptop.
    phone
        .send(Command::Start {
            session_id: SessionId::new("to-laptop").unwrap(),
            node_id: laptop_id.clone(),
            remote: SessionMedia::Speak {
                destination_id: "output:speakers".into(),
            },
            local: LocalMedia::Send {
                source_id: "input:mic".into(),
            },
        })
        .unwrap();
    next_event(
        &mut phone_events,
        "phone to laptop connected",
        active("to-laptop"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (level, hz) = analyse(&laptop_audio.played);
    println!("the laptop (not sharing) plays the phone: {level:.1} dBFS at {hz:.0} Hz");
    assert!(
        level > -20.0 && (hz - TONE_HZ).abs() < 15.0,
        "{level:.1} dBFS at {hz:.0} Hz (laptop: {}; phone: {}; laptop receiver: {})",
        laptop_audio.timing(),
        phone_audio.timing(),
        latest_diagnostics(&mut laptop_events)
    );

    // ...and listens to the phone.
    laptop
        .send(Command::Start {
            session_id: SessionId::new("laptop-listens").unwrap(),
            node_id: phone_id.clone(),
            remote: SessionMedia::Listen {
                source_id: "input:mic".into(),
            },
            local: LocalMedia::Receive {
                destination_id: "output:speakers".into(),
            },
        })
        .unwrap();
    next_event(
        &mut laptop_events,
        "laptop listens to the phone",
        active("laptop-listens"),
    )
    .await;

    // 5. Sharing: now it may be listened to.
    laptop.send(Command::SetSharing { sharing: true }).unwrap();
    next_event(&mut phone_events, "the laptop sharing", seen(true)).await;
    phone.send(listen_to_laptop("listen-shared")).unwrap();
    next_event(
        &mut phone_events,
        "listening to the laptop",
        active("listen-shared"),
    )
    .await;
    phone_audio.played.lock().unwrap().clear();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (level, hz) = analyse(&phone_audio.played);
    println!("the phone hears the laptop (sharing): {level:.1} dBFS at {hz:.0} Hz");
    assert!(
        level > -20.0 && (hz - TONE_HZ).abs() < 15.0,
        "{level:.1} dBFS at {hz:.0} Hz (phone: {}; laptop: {}; phone receiver: {})",
        phone_audio.timing(),
        laptop_audio.timing(),
        latest_diagnostics(&mut phone_events)
    );

    // 6. Stopping sharing ends what it sends, on the phone too; what it
    //    receives goes on.
    laptop.send(Command::SetSharing { sharing: false }).unwrap();
    let e = next_event(
        &mut phone_events,
        "the listening stream ended",
        ended("listen-shared"),
    )
    .await;
    let AppEvent::SessionEnded { reason, .. } = e else {
        unreachable!()
    };
    assert!(reason.contains("stopped sharing"), "{reason}");
    next_event(
        &mut phone_events,
        "the laptop not sharing again",
        seen(false),
    )
    .await;
    laptop_audio.played.lock().unwrap().clear();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (level, _) = analyse(&laptop_audio.played);
    assert!(
        level > -20.0,
        "the laptop still plays the phone: {level:.1} dBFS"
    );
    let _ = stop_tx.send(true);
}

/// Deletes devices from an account on drop (a live test's clean-up).
struct RemoveDevices {
    base: String,
    user: String,
    password: String,
    ids: Vec<String>,
}

impl Drop for RemoveDevices {
    fn drop(&mut self) {
        let (b, u, p, ids) = (
            self.base.clone(),
            self.user.clone(),
            self.password.clone(),
            std::mem::take(&mut self.ids),
        );
        let _ = std::thread::spawn(move || {
            let Ok(mut resp) = ureq::post(&format!("{b}/api/v1/login"))
                .send_json(serde_json::json!({ "username": u, "password": p }))
            else {
                eprintln!("clean-up: could not sign in to remove the test devices {ids:?}");
                return;
            };
            let body: serde_json::Value = resp.body_mut().read_json().unwrap_or_default();
            let token = body["token"].as_str().unwrap_or_default().to_owned();
            for id in ids {
                let removed = ureq::delete(&format!("{b}/api/v1/nodes/{id}"))
                    .header("Authorization", &format!("Bearer {token}"))
                    .call()
                    .is_ok();
                if !removed {
                    eprintln!("clean-up: could not remove the test device {id}");
                }
            }
        })
        .join();
    }
}

/// The TURN relay, for real: two temporary devices sign in to a live
/// AudioNet server, both limited to relayed candidates, and the phone
/// listens to the PC through the server's TURN relay. The devices are
/// removed afterwards.
/// `AUDIONET_URL=https://… AUDIONET_USER=… AUDIONET_PASSWORD=… cargo test -p audionet-server --test native_devices -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a live server with a TURN relay (AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD)"]
async fn audio_flows_through_a_real_turn_relay() {
    let base = std::env::var("AUDIONET_URL")
        .unwrap()
        .trim_end_matches('/')
        .to_owned();
    let user = std::env::var("AUDIONET_USER").unwrap();
    let password = std::env::var("AUDIONET_PASSWORD").unwrap();
    let pc_config = sign_in_as(&base, &user, &password, "Relay test PC");
    let phone_config = sign_in_as(&base, &user, &password, "Relay test phone");
    // Removes the temporary devices from the account when the test ends,
    // including when an assertion fails.
    let _cleanup = RemoveDevices {
        base: base.clone(),
        user: user.clone(),
        password: password.clone(),
        ids: vec![pc_config.node_id.clone(), phone_config.node_id.clone()],
    };
    let pc_id = NodeId::new(pc_config.node_id.clone()).unwrap();

    let pc_audio = Arc::new(FakeAudio::default());
    let phone_audio = Arc::new(FakeAudio::default());
    let (event_tx, mut events) = mpsc::unbounded_channel();
    let (control, commands) = Control::new(Arc::new(move |e| {
        let _ = event_tx.send(e);
    }));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    for (config, audio, control) in [
        (pc_config, Arc::clone(&pc_audio), None),
        (phone_config, Arc::clone(&phone_audio), Some(control)),
    ] {
        let mut stop = stop_rx.clone();
        // AUDIONET_RELAY_ONLY=0 compares with the direct path.
        // AUDIONET_RELAY_SIDE=pc or phone limits relay-only to one device.
        let side = std::env::var("AUDIONET_RELAY_SIDE").unwrap_or_default();
        let is_pc = control.is_none();
        let relay_only = std::env::var("AUDIONET_RELAY_ONLY").map_or(true, |v| v != "0")
            && (side.is_empty() || (side == "pc") == is_pc);
        tokio::spawn(
            agent_with(config, audio, control, relay_only).run(async move {
                let _ = stop.changed().await;
            }),
        );
    }
    let outcome = async {
        next_event(&mut events, "the PC online", |e| match e {
            AppEvent::Devices(nodes) => nodes
                .iter()
                .any(|n| n.name == "Relay test PC" && n.online && !n.sources.is_empty()),
            AppEvent::DeviceUpdate(n) => {
                n.name == "Relay test PC" && n.online && !n.sources.is_empty()
            }
            _ => false,
        })
        .await;
        let listen = SessionId::new("relay-listen").unwrap();
        commands
            .send(Command::Start {
                session_id: listen.clone(),
                node_id: pc_id,
                remote: SessionMedia::Listen {
                    source_id: "input:mic".into(),
                },
                local: LocalMedia::Receive {
                    destination_id: "output:speakers".into(),
                },
            })
            .unwrap();
        let mut network = Vec::new();
        loop {
            match next_event(&mut events, "relay session", |e| {
                matches!(e, AppEvent::Session { session_id, .. } if *session_id == listen)
                    || matches!(e, AppEvent::SessionEnded { .. })
            })
            .await
            {
                AppEvent::Session {
                    state: SessionState::Active,
                    ..
                } => break,
                AppEvent::Session { detail, .. } => {
                    println!("  {detail}");
                    network.push(detail);
                }
                other => panic!("session ended: {other:?}"),
            }
        }
        if std::env::var("AUDIONET_RELAY_ONLY").map_or(true, |v| v != "0") {
            assert!(
                network
                    .iter()
                    .any(|d| d.contains("relay address") && d.contains("(relay only)")),
                "no relayed candidate: {network:?}"
            );
        }
        let mut samples = Vec::new();
        for second in 1..=8 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let (level, hz) = analyse(&phone_audio.played);
            while let Ok(e) = events.try_recv() {
                if let AppEvent::Diagnostics { text, .. } = e {
                    println!("  {text}");
                }
            }
            let gaps = silent_runs(&phone_audio.played);
            println!(
                "  after {second} s: {level:.1} dBFS at {hz:.0} Hz, {gaps} silent gaps in the last second"
            );
            samples.push((level, hz));
        }
        // Steady state: the median of seconds 3 to 8 (the adaptive buffer
        // may still re-buffer once or twice early on a jittery route).
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let level = median(samples[2..].iter().map(|s| s.0).collect());
        let hz = median(samples[2..].iter().map(|s| s.1).collect());
        println!(
            "through the TURN relay: {level:.1} dBFS at {hz:.0} Hz (median of seconds 3 to 8)"
        );
        assert!(level > -20.0, "heard {level:.1} dBFS");
        assert!((hz - TONE_HZ).abs() < 15.0, "heard {hz:.0} Hz");
        commands.send(Command::Stop { session_id: listen }).unwrap();
    };
    let result = tokio::time::timeout(Duration::from_secs(60), outcome).await;
    let _ = stop_tx.send(true);
    result.expect("the relay test timed out");
}
