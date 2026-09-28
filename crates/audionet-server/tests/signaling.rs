//! End-to-end server test over real HTTP and WebSocket connections.

use std::sync::Arc;
use std::time::Duration;

use audionet_protocol::signal::{
    ClientInfo, ClientKind, ClientMessage, DestinationInfo, ServerMessage, SessionMedia,
    SourceInfo, SourceType,
};
use audionet_protocol::{NodeId, PROTOCOL_VERSION, Platform, SessionId};
use audionet_server::api::{AppState, router};
use audionet_server::config::Config;
use audionet_server::mail::Mailer;
use audionet_server::{auth, db};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start_server() -> (String, Arc<AppState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = Config::parse(&format!(
        "public_url = \"http://{addr}\"\n[ice]\nstun_urls = [\"stun:stun.example.com:3478\"]\nturn_urls = [\"turn:turn.example.com:3478\"]\nturn_secret = \"test-secret\""
    ))
    .unwrap();
    config.validate().unwrap();
    let db = db::Db::open_in_memory().unwrap();
    let hash = auth::hash_password("correct horse battery").unwrap();
    db.with(|c| db::create_user(c, "alice", &hash)).unwrap();
    db.with(|c| db::create_user(c, "mallory", &hash)).unwrap();
    let state = Arc::new(AppState::new(config, db, Mailer::Disabled));
    let app = router(Arc::clone(&state));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("{addr}"), state)
}

/// Minimal HTTP/1.1 JSON POST over a raw socket (keeps dependencies small).
async fn post(
    addr: &str,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> (u16, Vec<(String, String)>, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await.unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).await.unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.to_owned()))
        .collect();
    (status, headers, body.to_owned())
}

async fn connect(addr: &str, headers: &[(&str, &str)]) -> Result<Ws, String> {
    let mut req = format!("ws://{addr}/api/v1/ws")
        .into_client_request()
        .unwrap();
    for (k, v) in headers {
        let name: tokio_tungstenite::tungstenite::http::HeaderName = k.parse().unwrap();
        req.headers_mut().insert(name, v.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
        .map_err(|e| e.to_string())
}

async fn send(ws: &mut Ws, m: &ClientMessage) {
    ws.send(Message::Text(serde_json::to_string(m).unwrap().into()))
        .await
        .unwrap();
}

async fn recv(ws: &mut Ws) -> ServerMessage {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

fn hello(kind: ClientKind) -> ClientMessage {
    ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        client: ClientInfo {
            kind,
            software: "test".into(),
            platform: Some(Platform::Windows),
        },
    }
}

#[tokio::test]
async fn browser_and_device_negotiate_through_the_hub() {
    let (addr, state) = start_server().await;
    let origin = format!("http://{addr}");

    // Sign in (wrong password first).
    let (st, _, _) = post(
        &addr,
        "/api/v1/login",
        r#"{"username":"alice","password":"nope"}"#,
        &[("Origin", &origin)],
    )
    .await;
    assert_eq!(st, 401);
    let (st, h, body) = post(
        &addr,
        "/api/v1/login",
        r#"{"username":"alice","password":"correct horse battery"}"#,
        &[("Origin", &origin)],
    )
    .await;
    assert_eq!(st, 200, "{body}");
    let cookie = h.iter().find(|(k, _)| k == "set-cookie").unwrap().1.clone();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let cookie = cookie.split(';').next().unwrap().to_owned();

    // A foreign origin cannot use the cookie.
    let (st, _, _) = post(
        &addr,
        "/api/v1/logout",
        "{}",
        &[("Origin", "https://evil.example"), ("Cookie", &cookie)],
    )
    .await;
    assert_eq!(st, 403);

    // A device signs in with the account password.
    let (st, _, body) = post(
        &addr,
        "/api/v1/nodes/sign-in",
        r#"{"username":"alice","password":"correct horse battery","name":"Studio PC","platform":"windows"}"#,
        &[],
    )
    .await;
    assert_eq!(st, 200, "{body}");
    let paired: serde_json::Value = serde_json::from_str(&body).unwrap();
    let token = paired["token"].as_str().unwrap().to_owned();
    let node_id = paired["node_id"].as_str().unwrap().to_owned();
    // Pairing codes no longer exist.
    let (st, _, _) = post(
        &addr,
        "/api/v1/pairing-codes",
        "{}",
        &[("Origin", &origin), ("Cookie", &cookie)],
    )
    .await;
    assert!(st == 404 || st == 405, "pairing codes are gone ({st})");

    // Browser connects (cookie + origin).
    assert!(
        connect(&addr, &[("Cookie", &cookie)]).await.is_err(),
        "WebSocket without Origin is refused"
    );
    let mut browser = connect(&addr, &[("Cookie", &cookie), ("Origin", &origin)])
        .await
        .unwrap();
    send(&mut browser, &hello(ClientKind::Browser)).await;
    let ServerMessage::Welcome {
        ice_servers,
        node_id: none,
        ..
    } = recv(&mut browser).await
    else {
        panic!("no welcome")
    };
    assert!(none.is_none());
    assert_eq!(ice_servers.len(), 2);
    assert!(
        ice_servers[1]
            .username
            .as_deref()
            .unwrap()
            .ends_with(":alice")
    );

    // Device connects with its token.
    let bearer = format!("Bearer {token}");
    let mut device = connect(&addr, &[("Authorization", &bearer)]).await.unwrap();
    send(&mut device, &hello(ClientKind::Node)).await;
    let ServerMessage::Welcome {
        node_id: Some(nid), ..
    } = recv(&mut device).await
    else {
        panic!("no welcome")
    };
    assert_eq!(nid.as_str(), node_id);
    assert!(matches!(recv(&mut browser).await, ServerMessage::NodeUpdate { node } if node.online));

    send(
        &mut device,
        &ClientMessage::Endpoints {
            sources: vec![SourceInfo {
                id: "loopback:1".into(),
                name: "Speakers".into(),
                source_type: SourceType::Loopback,
                is_default: true,
            }],
            destinations: vec![DestinationInfo {
                id: "out:1".into(),
                name: "Speakers".into(),
                is_default: true,
            }],
        },
    )
    .await;
    let ServerMessage::NodeUpdate { node } = recv(&mut browser).await else {
        panic!()
    };
    assert_eq!(node.sources[0].name, "Speakers");

    send(&mut browser, &ClientMessage::ListNodes).await;
    let ServerMessage::Nodes { nodes } = recv(&mut browser).await else {
        panic!()
    };
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].name, "Studio PC");

    // Offer → answer → end.
    let sid = SessionId::new("sess-1").unwrap();
    send(
        &mut browser,
        &ClientMessage::SessionOffer {
            session_id: sid.clone(),
            node_id: NodeId::new(node_id.clone()).unwrap(),
            media: SessionMedia::Listen {
                source_id: "loopback:1".into(),
            },
            sdp: "v=0 offer".into(),
        },
    )
    .await;
    let ServerMessage::SessionOffer { sdp, .. } = recv(&mut device).await else {
        panic!()
    };
    assert_eq!(sdp, "v=0 offer");
    send(
        &mut device,
        &ClientMessage::SessionAnswer {
            session_id: sid.clone(),
            sdp: "v=0 answer".into(),
        },
    )
    .await;
    let ServerMessage::SessionAnswer { sdp, .. } = recv(&mut browser).await else {
        panic!()
    };
    assert_eq!(sdp, "v=0 answer");
    send(
        &mut browser,
        &ClientMessage::SessionEnd {
            session_id: sid,
            reason: "done".into(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut device).await,
        ServerMessage::SessionEnd { .. }
    ));

    // Another user's browser cannot see or reach the device.
    let (_, h, _) = post(
        &addr,
        "/api/v1/login",
        r#"{"username":"mallory","password":"correct horse battery"}"#,
        &[("Origin", &origin)],
    )
    .await;
    let mcookie = h
        .iter()
        .find(|(k, _)| k == "set-cookie")
        .unwrap()
        .1
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mut mallory = connect(&addr, &[("Cookie", &mcookie), ("Origin", &origin)])
        .await
        .unwrap();
    send(&mut mallory, &hello(ClientKind::Browser)).await;
    let _ = recv(&mut mallory).await;
    send(
        &mut mallory,
        &ClientMessage::SessionOffer {
            session_id: SessionId::new("evil").unwrap(),
            node_id: NodeId::new(node_id).unwrap(),
            media: SessionMedia::Listen {
                source_id: "loopback:1".into(),
            },
            sdp: "x".into(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut mallory).await, ServerMessage::Error { code, .. } if code == "node_offline")
    );

    // A browser cannot pretend to be a device.
    let mut fake = connect(&addr, &[("Cookie", &cookie), ("Origin", &origin)])
        .await
        .unwrap();
    send(&mut fake, &hello(ClientKind::Node)).await;
    assert!(
        matches!(recv(&mut fake).await, ServerMessage::Error { code, .. } if code == "wrong_client_kind")
    );

    // Device disconnect ends up as offline for the browser.
    drop(device);
    let ServerMessage::NodeUpdate { node } = recv(&mut browser).await else {
        panic!()
    };
    assert!(!node.online);
    assert!(state.hub.connection_count() >= 2);
}

/// Native apps: sign in with the password as a device, see each other come
/// online, and start sessions device to device.
#[tokio::test]
async fn native_apps_sign_in_as_devices_and_reach_each_other() {
    let (addr, _state) = start_server().await;
    let sign_in = |name: &'static str, password: &'static str| {
        let addr = addr.clone();
        async move {
            post(
                &addr,
                "/api/v1/nodes/sign-in",
                &format!(
                    r#"{{"username":"alice","password":"{password}","name":"{name}","platform":"ios"}}"#
                ),
                &[],
            )
            .await
        }
    };
    let (st, _, body) = sign_in("iPhone", "wrong password").await;
    assert_eq!(st, 401, "{body}");
    let (st, _, body) = sign_in("iPhone", "correct horse battery").await;
    assert_eq!(st, 200, "{body}");
    let phone: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(phone["token"].as_str().unwrap().starts_with("ann_"));
    assert_eq!(phone["username"], "alice");
    let (_, _, body) = sign_in("Studio PC", "correct horse battery").await;
    let pc: serde_json::Value = serde_json::from_str(&body).unwrap();

    let device = |token: String| {
        let addr = addr.clone();
        async move {
            let mut ws = connect(&addr, &[("Authorization", &format!("Bearer {token}"))])
                .await
                .unwrap();
            send(&mut ws, &hello(ClientKind::Node)).await;
            assert!(matches!(recv(&mut ws).await, ServerMessage::Welcome { .. }));
            ws
        }
    };
    let mut phone_ws = device(phone["token"].as_str().unwrap().to_owned()).await;
    let mut pc_ws = device(pc["token"].as_str().unwrap().to_owned()).await;

    // The phone app learns that the PC came online, and can list devices.
    let ServerMessage::NodeUpdate { node } = recv(&mut phone_ws).await else {
        panic!("no update")
    };
    assert!(node.online && node.name == "Studio PC");
    send(&mut phone_ws, &ClientMessage::ListNodes).await;
    let ServerMessage::Nodes { nodes } = recv(&mut phone_ws).await else {
        panic!()
    };
    assert_eq!(nodes.len(), 2);

    // The phone listens to the PC: device-to-device offer and answer.
    let sid = SessionId::new("d2d-1").unwrap();
    send(
        &mut phone_ws,
        &ClientMessage::SessionOffer {
            session_id: sid.clone(),
            node_id: NodeId::new(pc["node_id"].as_str().unwrap()).unwrap(),
            media: SessionMedia::Listen {
                source_id: "loopback:1".into(),
            },
            sdp: "v=0 phone offer".into(),
        },
    )
    .await;
    let ServerMessage::SessionOffer { sdp, .. } = recv(&mut pc_ws).await else {
        panic!()
    };
    assert_eq!(sdp, "v=0 phone offer");
    send(
        &mut pc_ws,
        &ClientMessage::SessionAnswer {
            session_id: sid.clone(),
            sdp: "v=0 pc answer".into(),
        },
    )
    .await;
    let ServerMessage::SessionAnswer { sdp, .. } = recv(&mut phone_ws).await else {
        panic!()
    };
    assert_eq!(sdp, "v=0 pc answer");

    // Another user's device cannot reach alice's PC.
    let (_, _, body) = post(
        &addr,
        "/api/v1/nodes/sign-in",
        r#"{"username":"mallory","password":"correct horse battery","name":"Intruder"}"#,
        &[],
    )
    .await;
    let m: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mut mallory = device(m["token"].as_str().unwrap().to_owned()).await;
    send(
        &mut mallory,
        &ClientMessage::SessionOffer {
            session_id: SessionId::new("evil").unwrap(),
            node_id: NodeId::new(pc["node_id"].as_str().unwrap()).unwrap(),
            media: SessionMedia::Listen {
                source_id: "loopback:1".into(),
            },
            sdp: "v=0".into(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut mallory).await,
        ServerMessage::Error { code, .. } if code == "node_offline"
    ));
}
