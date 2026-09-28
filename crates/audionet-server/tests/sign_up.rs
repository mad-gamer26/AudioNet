//! Creating accounts over real HTTP (the web client's sign-up): closed by
//! default, the self-service rules, and the per-address and server-wide
//! limits. Devices only sign in to existing accounts.

use std::sync::Arc;

use audionet_server::api::{AppState, router};
use audionet_server::config::Config;
use audionet_server::db;
use tokio::net::TcpStream;

async fn start_server(extra_config: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config =
        Config::parse(&format!("public_url = \"http://{addr}\"\n{extra_config}")).unwrap();
    config.validate().unwrap();
    let state = Arc::new(AppState {
        config,
        db: db::Db::open_in_memory().unwrap(),
        hub: Default::default(),
        login_throttle: Default::default(),
        sign_up_limit: Default::default(),
    });
    let app = router(state);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    format!("{addr}")
}

/// Minimal HTTP/1.1 JSON POST; returns the status and body.
async fn post(addr: &str, path: &str, body: &str, headers: &[(&str, &str)]) -> (u16, String) {
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
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_owned())
}

fn account(name: &str) -> String {
    format!(r#"{{"username":"{name}","password":"correct horse battery"}}"#)
}

#[tokio::test]
async fn closed_unless_allowed() {
    let addr = start_server("").await;
    let (status, body) = post(&addr, "/api/v1/register", &account("alice"), &[]).await;
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("registration_closed"));
}

#[tokio::test]
async fn devices_cannot_create_accounts() {
    // Apps sign in to accounts made in the web client; there is no device
    // sign-up.
    let addr = start_server("allow_registration = true").await;
    let (status, _) = post(
        &addr,
        "/api/v1/nodes/sign-up",
        r#"{"username":"carol","password":"correct horse battery","name":"Test PC","platform":"windows"}"#,
        &[],
    )
    .await;
    assert!(status == 404 || status == 405, "status {status}");
}

#[tokio::test]
async fn rules_for_new_accounts() {
    let addr = start_server("allow_registration = true").await;
    let (status, body) = post(&addr, "/api/v1/register", &account("alice"), &[]).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""username":"alice""#));
    // Taken, whatever the capitals.
    let (status, body) = post(&addr, "/api/v1/register", &account("ALICE"), &[]).await;
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("username_taken"));
    // Reserved names and passwords containing the name.
    let (status, body) = post(&addr, "/api/v1/register", &account("Admin"), &[]).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("reserved"));
    let (status, body) = post(
        &addr,
        "/api/v1/register",
        r#"{"username":"bob","password":"bob-is-great-2026"}"#,
        &[],
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("must not contain the username"));
    let (status, _) = post(
        &addr,
        "/api/v1/register",
        r#"{"username":"bob","password":"short"}"#,
        &[],
    )
    .await;
    assert_eq!(status, 400);
    // The new account signs in normally.
    let (status, body) = post(&addr, "/api/v1/login", &account("alice"), &[]).await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn limits_per_address_from_the_proxy_header() {
    let addr = start_server(
        "allow_registration = true\nsign_ups_per_address_per_hour = 2\nsign_ups_per_hour = 3\nclient_address_header = \"X-Real-IP\"",
    )
    .await;
    let one = [("X-Real-IP", "198.51.100.7")];
    let other = [("X-Real-IP", "203.0.113.9")];
    assert_eq!(
        post(&addr, "/api/v1/register", &account("u1"), &one)
            .await
            .0,
        200
    );
    // A taken name does not use up the allowance.
    assert_eq!(
        post(&addr, "/api/v1/register", &account("u1"), &one)
            .await
            .0,
        409
    );
    assert_eq!(
        post(&addr, "/api/v1/register", &account("u2"), &one)
            .await
            .0,
        200
    );
    let (status, body) = post(&addr, "/api/v1/register", &account("u3"), &one).await;
    assert_eq!(status, 429, "{body}");
    assert!(body.contains("from your network"), "{body}");
    // Another address still can, until the server-wide limit.
    assert_eq!(
        post(&addr, "/api/v1/register", &account("u3"), &other)
            .await
            .0,
        200
    );
    let (status, body) = post(&addr, "/api/v1/register", &account("u4"), &other).await;
    assert_eq!(status, 429, "{body}");
    assert!(
        body.contains("many new accounts in the last hour"),
        "{body}"
    );
}

#[tokio::test]
async fn without_a_proxy_header_the_connection_address_counts() {
    let addr = start_server("allow_registration = true\nsign_ups_per_address_per_hour = 1").await;
    // A client-supplied header is ignored when none is configured.
    let spoofed = [("X-Real-IP", "198.51.100.7")];
    assert_eq!(
        post(&addr, "/api/v1/register", &account("v1"), &spoofed)
            .await
            .0,
        200
    );
    let other = [("X-Real-IP", "203.0.113.9")];
    assert_eq!(
        post(&addr, "/api/v1/register", &account("v2"), &other)
            .await
            .0,
        429
    );
}
