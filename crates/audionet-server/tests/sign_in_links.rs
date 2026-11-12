//! The administrator's one-time sign-in links (`audionet-server user link`)
//! over real HTTP: they sign one web browser in, once, with the session only
//! in the browser's cookie, and they never add a device.

use std::sync::Arc;

use audionet_server::api::{AppState, router};
use audionet_server::auth::{hash_password, hash_token};
use audionet_server::config::Config;
use audionet_server::db;
use audionet_server::mail::Mailer;
use tokio::net::TcpStream;

async fn start_server() -> (String, db::Db) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = Config::parse(&format!("public_url = \"http://{addr}\"\n")).unwrap();
    config.validate().unwrap();
    let database = db::Db::open_in_memory().unwrap();
    let state = Arc::new(AppState::new(config, database.clone(), Mailer::Disabled));
    let app = router(state);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    (format!("{addr}"), database)
}

/// Minimal HTTP/1.1 request; returns the status, the head and the body.
async fn request(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> (u16, String, String) {
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
    let mut resp = String::new();
    s.read_to_string(&mut resp).await.unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_owned(), body.to_owned())
}

/// The `audionet-server user link` command's work: a user and a link.
fn make_link(database: &db::Db, token: &str, ttl_s: i64) {
    database
        .with(|c| {
            let user = match db::user_by_name(c, "kim")? {
                Some(u) => u.id,
                None => {
                    db::create_user(c, "kim", &hash_password("correct horse battery").unwrap())?
                }
            };
            db::create_sign_in_link(c, &hash_token(token), user, ttl_s)
        })
        .unwrap();
}

fn session_cookie(head: &str) -> Option<String> {
    head.lines()
        .find_map(|l| {
            l.strip_prefix("set-cookie: ")
                .or_else(|| l.strip_prefix("Set-Cookie: "))
        })
        .and_then(|c| c.split(';').next())
        .map(str::to_owned)
}

#[tokio::test]
async fn a_link_signs_one_browser_in_once() {
    let (addr, database) = start_server().await;
    make_link(&database, "anl_first", 3600);
    let origin = format!("http://{addr}");
    let (status, head, body) = request(
        &addr,
        "POST",
        "/api/v1/login/link",
        r#"{"token":"anl_first"}"#,
        &[("Origin", &origin)],
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""username":"kim""#), "{body}");
    // The session is only in the cookie, so no app can take it as a token.
    assert!(!body.contains("ans_"), "{body}");
    let cookie = session_cookie(&head).expect("a session cookie");
    assert!(cookie.contains("ans_"), "{cookie}");
    assert!(head.to_ascii_lowercase().contains("httponly"), "{head}");
    // The browser is signed in.
    let (status, _, body) = request(&addr, "GET", "/api/v1/me", "", &[("Cookie", &cookie)]).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""username":"kim""#), "{body}");
    // The link works once.
    let (status, _, body) = request(
        &addr,
        "POST",
        "/api/v1/login/link",
        r#"{"token":"anl_first"}"#,
        &[("Origin", &origin)],
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("link_invalid"), "{body}");
    // Signing out ends that session like any other.
    let (status, _, _) = request(
        &addr,
        "POST",
        "/api/v1/logout",
        "",
        &[("Cookie", &cookie), ("Origin", &origin)],
    )
    .await;
    assert_eq!(status, 200);
    let (status, _, _) = request(&addr, "GET", "/api/v1/me", "", &[("Cookie", &cookie)]).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn only_web_pages_of_this_server_can_use_a_link() {
    let (addr, database) = start_server().await;
    make_link(&database, "anl_web", 3600);
    // Apps and devices send no Origin.
    let (status, _, body) = request(
        &addr,
        "POST",
        "/api/v1/login/link",
        r#"{"token":"anl_web"}"#,
        &[],
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("web_only"), "{body}");
    // Another site's page.
    let (status, _, body) = request(
        &addr,
        "POST",
        "/api/v1/login/link",
        r#"{"token":"anl_web"}"#,
        &[("Origin", "https://evil.example")],
    )
    .await;
    assert_eq!(status, 403, "{body}");
    // Neither used the link up.
    let origin = format!("http://{addr}");
    let (status, _, body) = request(
        &addr,
        "POST",
        "/api/v1/login/link",
        r#"{"token":"anl_web"}"#,
        &[("Origin", &origin)],
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn expired_and_unknown_links_are_refused_and_add_no_device() {
    let (addr, database) = start_server().await;
    make_link(&database, "anl_old", -1);
    let origin = format!("http://{addr}");
    for token in ["anl_old", "anl_never_made"] {
        let (status, _, body) = request(
            &addr,
            "POST",
            "/api/v1/login/link",
            &format!(r#"{{"token":"{token}"}}"#),
            &[("Origin", &origin)],
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(body.contains("link_invalid"), "{body}");
    }
    // Device sign-in does not take a link in place of the password.
    make_link(&database, "anl_device", 3600);
    let (status, _, body) = request(
        &addr,
        "POST",
        "/api/v1/nodes/sign-in",
        r#"{"username":"kim","password":"anl_device","name":"Test PC","platform":"windows"}"#,
        &[],
    )
    .await;
    assert_eq!(status, 401, "{body}");
    let devices = database
        .with(|c| {
            let u = db::user_by_name(c, "kim")?.unwrap();
            db::nodes_for_user(c, u.id)
        })
        .unwrap();
    assert!(devices.is_empty());
}
