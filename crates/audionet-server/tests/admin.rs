//! The admin page over real HTTP and WebSocket connections: its guard and
//! optional password, accounts, devices and browsers seen live, streams,
//! and that removing things disconnects them at once.

use std::sync::{Arc, Once};
use std::time::Duration;

use audionet_protocol::signal::{
    ClientInfo, ClientKind, ClientMessage, ServerMessage, SessionMedia, SourceInfo, SourceType,
};
use audionet_protocol::{NodeId, PROTOCOL_VERSION, Platform, SessionId};
use audionet_server::api::{AppState, router};
use audionet_server::config::Config;
use audionet_server::mail::Mailer;
use audionet_server::{admin, auth, db};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const PW: &str = "correct horse battery";

fn log_to_buffer() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        use tracing_subscriber::layer::SubscriberExt as _;
        let sub = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("audionet_server=info"))
            .with(audionet_server::logbuf::BufferLayer);
        let _ = tracing::subscriber::set_global_default(sub);
    });
}

struct Servers {
    public: String,
    admin: String,
    state: Arc<AppState>,
}

async fn start(admin_extra: &str) -> Servers {
    log_to_buffer();
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let paddr = public.local_addr().unwrap();
    let aaddr = admin_l.local_addr().unwrap();
    let mut config = Config::parse(&format!(
        "public_url = \"http://{paddr}\"\n[admin]\nbind = \"{aaddr}\"\n{admin_extra}"
    ))
    .unwrap();
    config.validate().unwrap();
    let state = Arc::new(AppState::new(
        config,
        db::Db::open_in_memory().unwrap(),
        Mailer::Disabled,
    ));
    let app = router(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(
            public,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let adm = admin::router(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(
            admin_l,
            adm.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    Servers {
        public: paddr.to_string(),
        admin: aaddr.to_string(),
        state,
    }
}

/// Minimal HTTP/1.1 request; returns status, lowercased headers and body.
async fn http(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> (u16, Vec<(String, String)>, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let resp = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status = lines
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
    // Chunked bodies (axum's JSON): keep the JSON part.
    let body = match (body.find('{'), body.rfind('}')) {
        (Some(a), Some(b)) if headers_chunked(head) => body[a..=b].to_owned(),
        _ => body.to_owned(),
    };
    (status, headers, body)
}

fn headers_chunked(head: &str) -> bool {
    head.to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
}

impl Servers {
    async fn get(&self, path: &str) -> serde_json::Value {
        let (status, _, body) = http(&self.admin, "GET", path, "", &[]).await;
        assert_eq!(status, 200, "{path}: {body}");
        serde_json::from_str(&body).unwrap()
    }

    /// A change, as the page makes it.
    async fn post(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let origin = format!("http://{}", self.admin);
        let (status, _, b) = http(
            &self.admin,
            "POST",
            path,
            &body.to_string(),
            &[("Origin", &origin), ("X-AudioNet-Admin", "1")],
        )
        .await;
        (
            status,
            serde_json::from_str(&b).unwrap_or(serde_json::Value::Null),
        )
    }

    fn add_user(&self, name: &str) -> i64 {
        let hash = auth::hash_password(PW).unwrap();
        self.state
            .db
            .with(|c| db::create_user(c, name, &hash))
            .unwrap()
    }

    async fn sign_in_device(&self, user: &str, name: &str) -> String {
        let (status, _, body) = http(
            &self.public,
            "POST",
            "/api/v1/nodes/sign-in",
            &serde_json::json!({"username": user, "password": PW, "name": name, "platform": "windows"}).to_string(),
            &[],
        )
        .await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn browser_cookie(&self, user: &str) -> String {
        let origin = format!("http://{}", self.public);
        let (status, headers, body) = http(
            &self.public,
            "POST",
            "/api/v1/login",
            &serde_json::json!({"username": user, "password": PW}).to_string(),
            &[("Origin", &origin)],
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let cookie = headers
            .iter()
            .find(|(k, _)| k == "set-cookie")
            .unwrap()
            .1
            .clone();
        cookie.split(';').next().unwrap().to_owned()
    }

    async fn connect(&self, headers: &[(&str, &str)], kind: ClientKind) -> Result<Ws, String> {
        let mut req = format!("ws://{}/api/v1/ws", self.public)
            .into_client_request()
            .unwrap();
        for (k, v) in headers {
            let name: tokio_tungstenite::tungstenite::http::HeaderName = k.parse().unwrap();
            req.headers_mut().insert(name, v.parse().unwrap());
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(req)
            .await
            .map_err(|e| e.to_string())?;
        send(
            &mut ws,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                client: ClientInfo {
                    kind,
                    software: if kind == ClientKind::Node {
                        "audionet-desktop 9.9.9".into()
                    } else {
                        "audionet-web 0.1.0".into()
                    },
                    platform: Some(Platform::Windows),
                    visible: None,
                },
            },
        )
        .await;
        match recv(&mut ws).await {
            Some(ServerMessage::Welcome { .. }) => Ok(ws),
            other => Err(format!("no welcome: {other:?}")),
        }
    }

    async fn device(&self, token: &str) -> Result<Ws, String> {
        self.connect(
            &[("Authorization", &format!("Bearer {token}"))],
            ClientKind::Node,
        )
        .await
    }

    async fn browser(&self, cookie: &str) -> Result<Ws, String> {
        let origin = format!("http://{}", self.public);
        self.connect(
            &[("Cookie", cookie), ("Origin", &origin)],
            ClientKind::Browser,
        )
        .await
    }
}

async fn send(ws: &mut Ws, m: &ClientMessage) {
    ws.send(Message::Text(serde_json::to_string(m).unwrap().into()))
        .await
        .unwrap();
}

/// The next server message, or None once the connection has closed.
async fn recv(ws: &mut Ws) -> Option<ServerMessage> {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out")
        {
            Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(t.as_str()).unwrap()),
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
            Some(Ok(_)) => {}
        }
    }
}

/// Whether the server closed the connection (reading past queued messages).
async fn closed(ws: &mut Ws) -> bool {
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => return true,
            Ok(Some(Ok(_))) => continue,
            Err(_) => return false,
        }
    }
    false
}

#[tokio::test]
async fn the_page_and_its_guard() {
    let s = start("").await;
    let (status, headers, body) = http(&s.admin, "GET", "/", "", &[]).await;
    assert_eq!(status, 200);
    assert!(body.contains("AudioNet admin"));
    let h = |k: &str| {
        headers
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    assert!(h("content-security-policy").contains("frame-ancestors 'none'"));
    assert_eq!(h("cache-control"), "no-cache");
    for path in ["/app.js", "/style.css"] {
        assert_eq!(http(&s.admin, "GET", path, "", &[]).await.0, 200, "{path}");
    }
    let body = r#"{"username":"eve","password":"correct horse battery"}"#;
    let origin = format!("http://{}", s.admin);
    for headers in [
        vec![],
        vec![("Origin", origin.as_str())],
        vec![("X-AudioNet-Admin", "1")],
        vec![("Origin", "http://evil.example"), ("X-AudioNet-Admin", "1")],
    ] {
        let (status, _, b) = http(&s.admin, "POST", "/api/users", body, &headers).await;
        assert_eq!(status, 403, "{headers:?}: {b}");
    }
    assert!(
        s.state
            .db
            .with(|c| db::user_by_name(c, "eve"))
            .unwrap()
            .is_none()
    );
    // The public listener has no admin page.
    assert_eq!(
        http(&s.public, "GET", "/api/overview", "", &[]).await.0,
        404
    );
}

#[tokio::test]
async fn accounts() {
    let s = start("").await;
    let (st, r) = s
        .post(
            "/api/users",
            serde_json::json!({"username": "kim", "password": PW, "email": "kim@example.com"}),
        )
        .await;
    assert_eq!(st, 200, "{r}");
    let (st, r) = s
        .post(
            "/api/users",
            serde_json::json!({"username": "KIM", "password": PW}),
        )
        .await;
    assert_eq!(st, 409, "{r}");
    let (st, _) = s
        .post(
            "/api/users",
            serde_json::json!({"username": "lee", "password": "short"}),
        )
        .await;
    assert_eq!(st, 400);
    let (st, _) = s
        .post(
            "/api/users",
            serde_json::json!({"username": "a b/c", "password": PW}),
        )
        .await;
    assert_eq!(st, 400);
    let list = s.get("/api/users").await;
    let kim = &list["users"][0]["user"];
    assert_eq!(
        (kim["username"].as_str(), kim["email"].as_str()),
        (Some("kim"), Some("kim@example.com"))
    );

    // Email: another account can't take it; it can be removed.
    s.add_user("lee");
    let (st, r) = s
        .post(
            "/api/users/lee/email",
            serde_json::json!({"email": "kim@example.com"}),
        )
        .await;
    assert_eq!(st, 409, "{r}");
    let (st, _) = s
        .post("/api/users/kim/email", serde_json::json!({"email": null}))
        .await;
    assert_eq!(st, 200);
    assert!(
        s.state
            .db
            .with(|c| db::user_by_name(c, "kim"))
            .unwrap()
            .unwrap()
            .email
            .is_none()
    );

    // A sign-in link signs a browser in.
    let (st, r) = s
        .post("/api/users/kim/link", serde_json::json!({"hours": 2}))
        .await;
    assert_eq!(st, 200, "{r}");
    let link = r["link"].as_str().unwrap();
    let token = link.split("signin=").nth(1).unwrap();
    let origin = format!("http://{}", s.public);
    let (st, _, b) = http(
        &s.public,
        "POST",
        "/api/v1/login/link",
        &serde_json::json!({"token": token}).to_string(),
        &[("Origin", &origin)],
    )
    .await;
    assert_eq!(st, 200, "{b}");
    let (st, _) = s
        .post("/api/users/kim/link", serde_json::json!({"hours": 500}))
        .await;
    assert_eq!(st, 400);

    // A new password signs browsers out (and closes open ones).
    let cookie = s.browser_cookie("kim").await;
    let mut tab = s.browser(&cookie).await.unwrap();
    let (st, r) = s
        .post(
            "/api/users/kim/password",
            serde_json::json!({"password": "a brand new password"}),
        )
        .await;
    assert_eq!(st, 200, "{r}");
    assert!(closed(&mut tab).await, "the open browser closed");
    let (st, _, _) = http(&s.public, "GET", "/api/v1/me", "", &[("Cookie", &cookie)]).await;
    assert_eq!(st, 401);

    // Missing accounts say so.
    let (st, r) = s
        .post(
            "/api/users/nobody/password",
            serde_json::json!({"password": PW}),
        )
        .await;
    assert_eq!(st, 404);
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no longer exists")
    );

    // The log shows what the admin did.
    let log = s.get("/api/log?lines=2000").await;
    let lines: Vec<&str> = log["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l.as_str())
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("admin: created the account kim")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn devices_browsers_and_streams_live() {
    let s = start("").await;
    s.add_user("kim");
    let token = s.sign_in_device("kim", "Studio PC").await;
    let mut dev = s.device(&token).await.unwrap();
    send(
        &mut dev,
        &ClientMessage::Endpoints {
            sources: vec![SourceInfo {
                id: "src1".into(),
                name: "Speakers".into(),
                source_type: SourceType::Loopback,
                is_default: true,
            }],
            destinations: vec![],
        },
    )
    .await;
    let cookie = s.browser_cookie("kim").await;
    let mut tab = s.browser(&cookie).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let live = s.get("/api/live").await;
    let conns = live["connections"].as_array().unwrap();
    assert_eq!(conns.len(), 2, "{live}");
    let d = conns.iter().find(|c| c["kind"] == "device").unwrap();
    assert_eq!(d["username"], "kim");
    assert_eq!(d["node_name"], "Studio PC");
    assert_eq!(d["software"], "audionet-desktop 9.9.9");
    assert_eq!(d["address"], "127.0.0.1");
    assert_eq!(d["sources"], serde_json::json!(["Speakers"]));
    assert_eq!(d["visible"], true);
    let b = conns.iter().find(|c| c["kind"] == "browser").unwrap();
    assert_eq!(b["software"], "audionet-web 0.1.0");

    // A stream: the browser listens to the device's speakers.
    let node_id = d["node_id"].as_str().unwrap().to_owned();
    send(
        &mut tab,
        &ClientMessage::SessionOffer {
            session_id: SessionId::new("s_admin_test".to_owned()).unwrap(),
            node_id: NodeId::new(node_id.clone()).unwrap(),
            media: SessionMedia::Listen {
                source_id: "src1".into(),
            },
            sdp: "v=0".into(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let live = s.get("/api/live").await;
    let st = &live["sessions"][0];
    assert_eq!(
        (
            st["direction"].as_str(),
            st["endpoint"].as_str(),
            st["state"].as_str()
        ),
        (Some("listen"), Some("Speakers"), Some("starting")),
        "{live}"
    );

    // The account's page shows the device as connected, and the browser.
    let u = s.get("/api/users/kim").await;
    assert_eq!(u["devices"][0]["device"]["name"], "Studio PC");
    assert_eq!(u["devices"][0]["connection"]["address"], "127.0.0.1");
    assert_eq!(u["browsers"].as_array().unwrap().len(), 1);
    assert_eq!(u["web_sessions"].as_array().unwrap().len(), 1);

    // Disconnect: it goes, its stream ends, and it can come back.
    let conn = d["conn_id"].as_str().unwrap().to_owned();
    let (code, r) = s
        .post(
            &format!("/api/connections/{conn}/disconnect"),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(code, 200, "{r}");
    assert!(closed(&mut dev).await);
    assert!(
        s.get("/api/live").await["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut dev = s
        .device(&token)
        .await
        .expect("still signed in, so it reconnects");

    // Rename.
    let (code, _) = s
        .post(
            &format!("/api/devices/{node_id}/rename"),
            serde_json::json!({"name": "Mixing desk"}),
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(
        s.get("/api/users/kim").await["devices"][0]["device"]["name"],
        "Mixing desk"
    );

    // Remove: disconnected at once and can't come back.
    let (code, r) = s
        .post(
            &format!("/api/devices/{node_id}/delete"),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(code, 200, "{r}");
    assert!(closed(&mut dev).await);
    assert!(s.device(&token).await.is_err(), "its credential is gone");

    // Sign out every browser: the open one closes and its sign-in is gone.
    let (code, r) = s
        .post("/api/users/kim/sign-out-browsers", serde_json::json!({}))
        .await;
    assert_eq!(code, 200, "{r}");
    assert!(closed(&mut tab).await);
    assert!(s.browser(&cookie).await.is_err());
}

#[tokio::test]
async fn deleting_an_account_disconnects_everything() {
    let s = start("").await;
    s.add_user("kim");
    let token = s.sign_in_device("kim", "Laptop").await;
    let mut dev = s.device(&token).await.unwrap();
    let cookie = s.browser_cookie("kim").await;
    let mut tab = s.browser(&cookie).await.unwrap();
    let (code, r) = s.post("/api/users/kim/delete", serde_json::json!({})).await;
    assert_eq!(code, 200, "{r}");
    assert!(
        r["message"]
            .as_str()
            .unwrap()
            .contains("2 connections were closed"),
        "{r}"
    );
    assert!(closed(&mut dev).await);
    assert!(closed(&mut tab).await);
    assert!(
        s.get("/api/live").await["connections"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        s.get("/api/users").await["users"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_optional_password() {
    let hash = auth::hash_password("admin password 1").unwrap();
    let s = start(&format!("password_hash = \"{hash}\"")).await;
    let (st, headers, _) = http(&s.admin, "GET", "/", "", &[]).await;
    assert_eq!(st, 401);
    assert!(
        headers
            .iter()
            .any(|(k, v)| k == "www-authenticate" && v.contains("Basic"))
    );
    let good = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("admin:admin password 1")
    );
    let bad = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("admin:wrong")
    );
    assert_eq!(
        http(&s.admin, "GET", "/", "", &[("Authorization", &good)])
            .await
            .0,
        200
    );
    assert_eq!(
        http(
            &s.admin,
            "GET",
            "/api/users",
            "",
            &[("Authorization", &bad)]
        )
        .await
        .0,
        401
    );
    assert_eq!(
        http(
            &s.admin,
            "GET",
            "/api/users",
            "",
            &[("Authorization", &good)]
        )
        .await
        .0,
        200
    );
}

#[test]
fn reachable_from_elsewhere_only_with_a_password() {
    let base = "public_url = \"https://audionet.example\"\n[admin]\n";
    let mut c = Config::parse(&format!("{base}bind = \"0.0.0.0:8741\"")).unwrap();
    let e = c.validate().unwrap_err();
    assert!(e.contains("password_hash"), "{e}");
    let hash = auth::hash_password("admin password 1").unwrap();
    let mut c = Config::parse(&format!(
        "{base}bind = \"0.0.0.0:8741\"\npassword_hash = \"{hash}\""
    ))
    .unwrap();
    c.validate().unwrap();
    let mut c = Config::parse(&format!("{base}bind = \"0.0.0.0:8741\"\nenabled = false")).unwrap();
    c.validate().unwrap();
    let mut c = Config::parse(&format!("{base}password_hash = \"plain\"")).unwrap();
    assert!(c.validate().is_err());
    // Not mentioned at all: on, this machine only.
    let mut c = Config::parse("public_url = \"https://audionet.example\"").unwrap();
    c.validate().unwrap();
    assert!(c.admin.enabled);
    assert_eq!(c.admin.bind.to_string(), "127.0.0.1:8741");
}

/// Regression: removing a device in the web client (the public API) used to
/// forget it in the hub but leave its WebSocket open.
#[tokio::test]
async fn removing_a_device_in_the_web_client_disconnects_it() {
    let s = start("").await;
    s.add_user("kim");
    let token = s.sign_in_device("kim", "Old phone").await;
    let mut dev = s.device(&token).await.unwrap();
    let node_id = s.get("/api/live").await["connections"][0]["node_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cookie = s.browser_cookie("kim").await;
    let origin = format!("http://{}", s.public);
    let (st, _, b) = http(
        &s.public,
        "DELETE",
        &format!("/api/v1/nodes/{node_id}"),
        "",
        &[("Cookie", &cookie), ("Origin", &origin)],
    )
    .await;
    assert_eq!(st, 200, "{b}");
    assert!(closed(&mut dev).await, "the removed device is disconnected");
}

/// Regression: a device that reconnects replaces its older connection,
/// which now really closes.
#[tokio::test]
async fn a_newer_connection_of_a_device_closes_the_older_one() {
    let s = start("").await;
    s.add_user("kim");
    let token = s.sign_in_device("kim", "Laptop").await;
    let mut first = s.device(&token).await.unwrap();
    let _second = s.device(&token).await.unwrap();
    assert!(closed(&mut first).await);
    assert_eq!(
        s.get("/api/live").await["connections"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
