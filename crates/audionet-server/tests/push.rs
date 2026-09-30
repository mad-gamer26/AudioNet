//! Push notifications through a real server that is also its own push
//! gateway, with Apple's push service replaced by a recorder: a phone turns
//! notifications on, another device of the account comes online, changes
//! its sharing and goes away, and what would reach Apple is decrypted with
//! the phone's key.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use audionet_server::api::{AppState, router};
use audionet_server::apns::{Apns, Sent};
use audionet_server::config::Config;
use audionet_server::mail::Mailer;
use audionet_server::push::PushTiming;
use audionet_server::{auth, db};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use futures_util::SinkExt;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const KEY: [u8; 32] = [42; 32];

async fn start() -> (String, Arc<AppState>, Arc<Mutex<Vec<Sent>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = Config::parse(&format!(
        "public_url = \"http://{addr}\"\npush_gateway_url = \"http://{addr}\"\n"
    ))
    .unwrap();
    config.validate().unwrap();
    let db = db::Db::open_in_memory().unwrap();
    let hash = auth::hash_password("correct horse battery").unwrap();
    db.with(|c| db::create_user(c, "alice", &hash)).unwrap();
    let (apns, sent) = Apns::memory();
    let mut state = AppState::new(config, db, Mailer::Disabled);
    state.apns = Some(apns);
    state.push_timing = PushTiming {
        offline_grace: Duration::from_secs(1),
        startup_quiet: Duration::ZERO,
    };
    let state = Arc::new(state);
    audionet_server::push::start(Arc::clone(&state));
    let app = router(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    (format!("{addr}"), state, sent)
}

/// Minimal HTTP/1.1 request; returns the status and body.
async fn request(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    bearer: Option<&str>,
) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(t) = bearer {
        req.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await.unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).await.unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    (
        head.split(' ').nth(1).unwrap().parse().unwrap(),
        body.to_owned(),
    )
}

async fn sign_in_device(addr: &str, name: &str) -> String {
    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/nodes/sign-in",
        &format!(r#"{{"username":"alice","password":"correct horse battery","name":"{name}","platform":"windows"}}"#),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["token"].as_str().unwrap().to_owned()
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// A device's signaling connection (what the apps keep open).
async fn go_online(addr: &str, token: &str) -> Ws {
    let mut req = format!("ws://{addr}/api/v1/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws.send(Message::Text(
        r#"{"type":"hello","protocol_version":{"major":0,"minor":1},"client":{"kind":"node","software":"test","platform":"windows"}}"#.into(),
    ))
    .await
    .unwrap();
    ws
}

async fn set_sharing(ws: &mut Ws, on: bool) {
    ws.send(Message::Text(
        format!(r#"{{"type":"sharing","sharing":{on}}}"#).into(),
    ))
    .await
    .unwrap();
}

/// Waits for `n` notifications and returns their decrypted title and body.
async fn received(sent: &Mutex<Vec<Sent>>, n: usize) -> Vec<(String, String, String)> {
    for _ in 0..100 {
        if sent.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    sent.lock()
        .unwrap()
        .iter()
        .map(|s| {
            assert_eq!(s.payload["aps"]["mutable-content"], 1);
            assert_eq!(
                s.payload["aps"]["alert"]["body"], "A device changed.",
                "placeholder only"
            );
            let sealed = STANDARD.decode(s.payload["e"].as_str().unwrap()).unwrap();
            let (nonce, rest) = sealed.split_at(12);
            let nonce: [u8; 12] = nonce.try_into().unwrap();
            let plain = ChaCha20Poly1305::new_from_slice(&KEY)
                .unwrap()
                .decrypt(&Nonce::from(nonce), rest)
                .unwrap();
            let v: serde_json::Value = serde_json::from_slice(&plain).unwrap();
            (
                v["title"].as_str().unwrap().to_owned(),
                v["body"].as_str().unwrap().to_owned(),
                s.collapse_id.clone(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_phone_hears_about_the_accounts_other_devices() {
    let (addr, state, sent) = start().await;
    let phone = sign_in_device(&addr, "iPhone").await;
    let pc = sign_in_device(&addr, "Studio PC").await;

    // The server tells apps its gateway.
    let (_, info) = request(&addr, "GET", "/api/v1/info", "", None).await;
    assert!(
        info.contains(&format!(r#""push_gateway":"http://{addr}""#)),
        "{info}"
    );

    // The phone trades its Apple token for a handle at the gateway...
    let (status, body) = request(
        &addr,
        "POST",
        "/push/v1/register",
        r#"{"apns_token":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","sandbox":true}"#,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let handle = serde_json::from_str::<serde_json::Value>(&body).unwrap()["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    // ...and turns notifications on with its server, only through that
    // gateway.
    let pusher = |gateway: &str, presence: bool, sharing: bool| {
        format!(
            r#"{{"gateway":"{gateway}","handle":"{handle}","key":"{}","presence":{presence},"sharing":{sharing}}}"#,
            STANDARD.encode(KEY)
        )
    };
    let (status, body) = request(
        &addr,
        "PUT",
        "/api/v1/push/pusher",
        &pusher("https://elsewhere.example", true, true),
        Some(&phone),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("only through"), "{body}");
    let (status, body) = request(
        &addr,
        "PUT",
        "/api/v1/push/pusher",
        &pusher(&format!("http://{addr}/"), true, true),
        Some(&phone),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    // The phone itself coming online says nothing to itself.
    let _phone_ws = go_online(&addr, &phone).await;
    // The PC comes online, reports its sharing (nothing), then stops
    // sharing, then starts again, then goes away for good.
    let mut pc_ws = go_online(&addr, &pc).await;
    set_sharing(&mut pc_ws, true).await;
    tokio::time::sleep(Duration::from_millis(2300)).await;
    set_sharing(&mut pc_ws, false).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    set_sharing(&mut pc_ws, true).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(pc_ws);

    let got = received(&sent, 4).await;
    let bodies: Vec<&str> = got.iter().map(|(_, b, _)| b.as_str()).collect();
    assert_eq!(
        bodies,
        [
            "Studio PC is online.",
            "Studio PC stopped sharing its audio.",
            "Studio PC started sharing its audio.",
            "Studio PC is offline.",
        ]
    );
    assert!(
        got.iter().all(|(title, _, _)| title == "alice"),
        "the account name is the title"
    );
    assert!(got[0].2.starts_with("online-") && got[1].2.starts_with("sharing-"));
    assert!(
        sent.lock().unwrap().iter().all(|s| s.sandbox),
        "the phone's environment"
    );

    // Presence only: sharing changes are not sent.
    sent.lock().unwrap().clear();
    request(
        &addr,
        "PUT",
        "/api/v1/push/pusher",
        &pusher(&format!("http://{addr}"), true, false),
        Some(&phone),
    )
    .await;
    let mut pc_ws = go_online(&addr, &pc).await;
    set_sharing(&mut pc_ws, true).await;
    tokio::time::sleep(Duration::from_millis(2300)).await;
    set_sharing(&mut pc_ws, false).await;
    let got = received(&sent, 1).await;
    assert_eq!(
        got.iter().map(|g| g.1.as_str()).collect::<Vec<_>>(),
        ["Studio PC is online."]
    );

    // Both off: the pusher is gone.
    request(
        &addr,
        "PUT",
        "/api/v1/push/pusher",
        &pusher(&format!("http://{addr}"), false, false),
        Some(&phone),
    )
    .await;
    let count = state
        .db
        .with(|c| c.query_row("SELECT COUNT(*) FROM pushers", [], |r| r.get::<_, i64>(0)))
        .unwrap();
    assert_eq!(count, 0);
    drop(pc_ws);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_phone_without_the_app_is_forgotten() {
    let (addr, state, sent) = start().await;
    let phone = sign_in_device(&addr, "iPhone").await;
    let pc = sign_in_device(&addr, "Studio PC").await;
    // Apple says this token is no longer valid (the app was removed).
    let (_, body) = request(
        &addr,
        "POST",
        "/push/v1/register",
        r#"{"apns_token":"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"}"#,
        None,
    )
    .await;
    let handle = serde_json::from_str::<serde_json::Value>(&body).unwrap()["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _) = request(
        &addr,
        "PUT",
        "/api/v1/push/pusher",
        &format!(
            r#"{{"gateway":"http://{addr}","handle":"{handle}","key":"{}","presence":true,"sharing":false}}"#,
            STANDARD.encode(KEY)
        ),
        Some(&phone),
    )
    .await;
    assert_eq!(status, 200);
    let _pc_ws = go_online(&addr, &pc).await;
    // The gateway drops the handle and the server its pusher.
    let mut gone = false;
    for _ in 0..60 {
        let n = state
            .db
            .with(|c| c.query_row("SELECT COUNT(*) FROM pushers", [], |r| r.get::<_, i64>(0)))
            .unwrap();
        if n == 0 {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gone, "the pusher was not forgotten");
    assert!(sent.lock().unwrap().is_empty());
    let (status, _) = request(
        &addr,
        "POST",
        "/push/v1/send",
        &format!(r#"{{"handle":"{handle}","payload":"x","collapse_id":"c"}}"#),
        None,
    )
    .await;
    assert_eq!(status, 410, "the handle is gone at the gateway too");
}

/// The real APNs sender over HTTP/2, against a stand-in for Apple's service
/// on this machine: the path, headers and signed provider token are what
/// Apple expects, and "410 Unregistered" and "BadDeviceToken" count as
/// gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apns_over_http2() {
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};

    type Seen = Arc<Mutex<Vec<(String, hyper::HeaderMap, serde_json::Value)>>>;
    let seen: Seen = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let recorder = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let recorder = Arc::clone(&recorder);
            tokio::spawn(async move {
                let service = hyper::service::service_fn(
                    move |req: hyper::Request<hyper::body::Incoming>| {
                        let recorder = Arc::clone(&recorder);
                        async move {
                            let path = req.uri().path().to_owned();
                            let headers = req.headers().clone();
                            let body = req.into_body().collect().await.unwrap().to_bytes();
                            recorder.lock().unwrap().push((
                                path.clone(),
                                headers,
                                serde_json::from_slice(&body).unwrap(),
                            ));
                            let (status, reason) = if path.ends_with("/dead00") {
                                (410, "Unregistered")
                            } else if path.ends_with("/bad000") {
                                (400, "BadDeviceToken")
                            } else {
                                (200, "")
                            };
                            Ok::<_, std::convert::Infallible>(
                                hyper::Response::builder()
                                    .status(status)
                                    .body(Full::new(Bytes::from(format!(
                                        r#"{{"reason":"{reason}"}}"#
                                    ))))
                                    .unwrap(),
                            )
                        }
                    },
                );
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(tcp), service)
                    .await;
            });
        }
    });

    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let dir = std::env::temp_dir().join(format!("audionet-apns-h2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key_file = dir.join("AuthKey_TEST.p8");
    std::fs::write(
        &key_file,
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(pkcs8.as_ref())
        ),
    )
    .unwrap();
    let apns = Apns::from_config(&audionet_server::config::PushGatewayConfig {
        apns_key_file: Some(key_file),
        apns_key_id: Some("KEYID12345".into()),
        apns_team_id: Some("TEAMID1234".into()),
        apns_topic: Some("com.example.AudioNet".into()),
        apns_url_override: Some(format!("http://{addr}")),
    })
    .unwrap()
    .unwrap();

    use audionet_server::apns::Outcome;
    let payload = serde_json::json!({ "aps": { "alert": { "title": "AudioNet" } }, "e": "sealed" });
    assert_eq!(
        apns.send("abc123", false, "online-node_x", payload.clone())
            .await,
        Outcome::Sent
    );
    assert_eq!(
        apns.send("dead00", false, "c", payload.clone()).await,
        Outcome::Unregistered
    );
    assert_eq!(
        apns.send("bad000", true, "c", payload.clone()).await,
        Outcome::Unregistered
    );

    let seen = seen.lock().unwrap();
    let (path, headers, body) = &seen[0];
    assert_eq!(path, "/3/device/abc123");
    assert_eq!(body, &payload);
    let h = |k: &str| headers.get(k).unwrap().to_str().unwrap().to_owned();
    assert_eq!(h("apns-topic"), "com.example.AudioNet");
    assert_eq!(h("apns-push-type"), "alert");
    assert_eq!(h("apns-priority"), "10");
    assert_eq!(h("apns-collapse-id"), "online-node_x");
    let jwt = h("authorization");
    let jwt = jwt.strip_prefix("bearer ").unwrap();
    let header: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(jwt.split('.').next().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (header["alg"].as_str(), header["kid"].as_str()),
        (Some("ES256"), Some("KEYID12345"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}
