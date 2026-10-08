//! A connection to the server that dies without a word (a network change
//! leaves it half open: the system still shows it open, but nothing moves
//! either way) is noticed by the device, which reconnects by itself.
//!
//! A proxy between the device and a real server forwards traffic until it
//! is told to swallow everything on the connections open so far, as a lost
//! route would. New connections pass normally, so the reconnect works.
//! Takes about a minute: the device waits 45 seconds of silence before
//! giving up on a connection, checked every 20 seconds.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audionet_audio::render::RenderSource;
use audionet_node::agent::{Agent, AppEvent, Control};
use audionet_node::audio::{NodeAudio, OpenCapture, StreamGuard};
use audionet_node::config::NodeConfig;
use audionet_protocol::Platform;
use audionet_protocol::signal::{DestinationInfo, SourceInfo};
use audionet_server::api::{AppState, router};
use audionet_server::config::Config;
use audionet_server::mail::Mailer;
use audionet_server::{auth, db};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// A device with no sound hardware: only its connection matters here.
struct NoAudio;

impl NodeAudio for NoAudio {
    fn endpoints(&self) -> Result<(Vec<SourceInfo>, Vec<DestinationInfo>), String> {
        Ok((Vec::new(), Vec::new()))
    }

    fn open_source(&self, _id: &str) -> Result<OpenCapture, String> {
        Err("no audio in this test".into())
    }

    fn open_destination(
        &self,
        _id: &str,
        _source: Box<dyn RenderSource>,
    ) -> Result<(Box<dyn StreamGuard>, String), String> {
        Err("no audio in this test".into())
    }
}

async fn start_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = Config::parse(&format!("public_url = \"http://{addr}\"\n")).unwrap();
    config.validate().unwrap();
    let db = db::Db::open_in_memory().unwrap();
    let hash = auth::hash_password("correct horse battery").unwrap();
    db.with(|c| db::create_user(c, "alice", &hash)).unwrap();
    let state = Arc::new(AppState::new(config, db, Mailer::Disabled));
    tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    format!("127.0.0.1:{}", addr.port())
}

/// Copies `from` to `to` until either closes; while `dead` is set, reads
/// and drops everything instead (a route that no longer delivers).
async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    dead: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if !dead.load(Relaxed) && to.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

/// A proxy to `server`. Returns its address and the switches of the
/// connections it has carried (set one to make that connection go silent).
async fn start_proxy(server: String) -> (String, Arc<Mutex<Vec<Arc<AtomicBool>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let switches: Arc<Mutex<Vec<Arc<AtomicBool>>>> = Arc::default();
    let all = Arc::clone(&switches);
    tokio::spawn(async move {
        loop {
            let (client, _) = listener.accept().await.unwrap();
            let upstream = TcpStream::connect(&server).await.unwrap();
            let dead = Arc::new(AtomicBool::new(false));
            all.lock().unwrap().push(Arc::clone(&dead));
            let (cr, cw) = client.into_split();
            let (ur, uw) = upstream.into_split();
            tokio::spawn(pump(cr, uw, Arc::clone(&dead)));
            tokio::spawn(pump(ur, cw, dead));
        }
    });
    (format!("http://{addr}"), switches)
}

fn sign_in(base: &str) -> NodeConfig {
    let base = base.to_owned();
    std::thread::spawn(move || {
        let body: serde_json::Value = ureq::post(&format!("{base}/api/v1/nodes/sign-in"))
            .send_json(serde_json::json!({
                "username": "alice", "password": "correct horse battery",
                "name": "Studio PC", "platform": "windows"
            }))
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        NodeConfig {
            server_url: base,
            node_id: body["node_id"].as_str().unwrap().into(),
            token: body["token"].as_str().unwrap().into(),
            name: "Studio PC".into(),
            username: body["username"].as_str().unwrap().into(),
        }
    })
    .join()
    .unwrap()
}

async fn next_event(
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    within: Duration,
    what: &str,
    pred: impl Fn(&AppEvent) -> bool,
) -> AppEvent {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let e = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap();
        if pred(&e) {
            return e;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silently_dead_connection_is_noticed_and_replaced() {
    let server = start_server().await;
    let (proxy, switches) = start_proxy(server).await;
    let config = sign_in(&proxy);

    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (control, _commands) = Control::new(Arc::new(move |e| {
        let _ = events_tx.send(e);
    }));
    let agent = Agent {
        config,
        audio: Arc::new(NoAudio),
        platform: Platform::Windows,
        software: "test".into(),
        status: Arc::new(|_| {}),
        thread_setup: None,
        active_sessions: Default::default(),
        control: Some(control),
        relay_only: false,
        sharing: Arc::new(AtomicBool::new(true)),
        visitor: false,
        measurements_in_status: Arc::new(AtomicBool::new(false)),
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let run = tokio::spawn(agent.run(async {
        let _ = stop_rx.await;
    }));

    next_event(
        &mut events,
        Duration::from_secs(10),
        "the first connection",
        |e| matches!(e, AppEvent::Connected { .. }),
    )
    .await;

    // Every connection so far goes silent both ways, still open.
    let died = Instant::now();
    for s in switches.lock().unwrap().iter() {
        s.store(true, Relaxed);
    }

    let lost = next_event(
        &mut events,
        Duration::from_secs(80),
        "the device to notice",
        |e| matches!(e, AppEvent::Disconnected { .. }),
    )
    .await;
    let noticed = died.elapsed();
    let AppEvent::Disconnected { reason } = lost else {
        unreachable!()
    };
    assert!(
        reason.contains("nothing from the server"),
        "unexpected reason: {reason}"
    );
    assert!(
        noticed >= Duration::from_secs(40) && noticed <= Duration::from_secs(70),
        "noticed after {noticed:?}"
    );

    // A new connection through the proxy works again.
    next_event(&mut events, Duration::from_secs(10), "the reconnect", |e| {
        matches!(e, AppEvent::Connected { .. })
    })
    .await;
    let reconnected = died.elapsed() - noticed;
    assert!(
        reconnected <= Duration::from_secs(5),
        "reconnected after {reconnected:?}"
    );

    let _ = stop_tx.send(());
    let _ = run.await;
}
