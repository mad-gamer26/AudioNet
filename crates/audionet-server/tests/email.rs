//! Email addresses over real HTTP: confirming an address, changing it, and
//! resetting a forgotten password, with the in-memory mailer standing in for
//! SMTP.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use audionet_server::api::{AppState, router};
use audionet_server::config::Config;
use audionet_server::mail::{Mailer, SentEmail};
use audionet_server::{auth, db};
use tokio::net::TcpStream;

type Outbox = Arc<Mutex<Vec<SentEmail>>>;

async fn start_server(mailer: Mailer) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = Config::parse(&format!(
        "public_url = \"http://{addr}/\"\nallow_registration = true"
    ))
    .unwrap();
    config.validate().unwrap();
    let db = db::Db::open_in_memory().unwrap();
    // An account from before email addresses, and one whose address an
    // administrator set (confirmed).
    let hash = auth::hash_password("correct horse battery").unwrap();
    db.with(|c| {
        db::create_user(c, "old", &hash)?;
        let id = db::create_user(c, "admin-made", &hash)?;
        db::set_email(c, id, Some("made@example.com"))
    })
    .unwrap();
    let app = router(Arc::new(AppState::new(config, db, mailer)));
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

struct Resp {
    status: u16,
    cookie: Option<String>,
    body: String,
}

/// Minimal HTTP/1.1 request; `cookie` is sent with the page's Origin, as a
/// browser does.
async fn request(addr: &str, method: &str, path: &str, body: &str, cookie: Option<&str>) -> Resp {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(c) = cookie {
        req.push_str(&format!("Cookie: {c}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await.unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).await.unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let cookie = head
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|v| v.split(';').next())
        .filter(|v| !v.ends_with('='))
        .map(str::to_owned);
    Resp {
        status,
        cookie,
        body: body.to_owned(),
    }
}

async fn post(addr: &str, path: &str, body: &str, cookie: Option<&str>) -> Resp {
    request(addr, "POST", path, body, cookie).await
}

/// Waits for the background sender, then takes everything sent.
async fn take_mail(outbox: &Outbox, expected: usize) -> Vec<SentEmail> {
    for _ in 0..100 {
        if outbox.lock().unwrap().len() >= expected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Give a wrongly sent extra message the chance to show up.
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::mem::take(&mut *outbox.lock().unwrap())
}

/// The token of the `param` link in an email.
fn link_token(mail: &SentEmail, param: &str) -> String {
    let start = mail.body.find(&format!("/?{param}=")).expect("has a link") + param.len() + 3;
    mail.body[start..]
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn sign_up_confirms_the_address() {
    let (mailer, outbox) = Mailer::memory();
    let addr = start_server(mailer).await;
    let info = request(&addr, "GET", "/api/v1/info", "", None).await;
    assert!(
        info.body.contains(r#""password_reset":true"#),
        "{}",
        info.body
    );

    let r = post(
        &addr,
        "/api/v1/register",
        r#"{"username":"alice","password":"correct horse battery","email":"Alice@Example.com"}"#,
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let session = r.cookie.unwrap();
    let me = request(&addr, "GET", "/api/v1/me", "", Some(&session)).await;
    assert!(
        me.body.contains(r#""email":null"#)
            && me.body.contains(r#""pending_email":"Alice@Example.com""#),
        "waiting for confirmation: {}",
        me.body
    );

    let mail = take_mail(&outbox, 1).await;
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].to, "Alice@Example.com");
    assert!(mail[0].subject.contains("Confirm"));
    assert!(mail[0].body.contains("Hello alice"));
    assert!(
        mail[0]
            .body
            .contains(&format!("http://{addr}/?verify=anv_")),
        "no doubled slash: {}",
        mail[0].body
    );
    let token = link_token(&mail[0], "verify");

    // The link works without being signed in (it may open on another
    // device), and only once.
    let body = format!(r#"{{"token":"{token}"}}"#);
    let r = post(&addr, "/api/v1/email/verify", &body, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains(r#""username":"alice""#));
    assert!(
        r.body.contains(r#""email":"Alice@Example.com""#),
        "{}",
        r.body
    );
    let again = post(&addr, "/api/v1/email/verify", &body, None).await;
    assert_eq!(again.status, 400);
    assert!(again.body.contains("link_invalid"));
    let me = request(&addr, "GET", "/api/v1/me", "", Some(&session)).await;
    assert!(
        me.body.contains(r#""email":"Alice@Example.com""#)
            && me.body.contains(r#""pending_email":null"#),
        "{}",
        me.body
    );
    // Nothing more to confirm.
    let r = post(&addr, "/api/v1/account/email/send-link", "", Some(&session)).await;
    assert_eq!(r.status, 200);
    assert!(take_mail(&outbox, 0).await.is_empty());
}

#[tokio::test]
async fn adding_an_address_to_an_older_account() {
    let (mailer, outbox) = Mailer::memory();
    let addr = start_server(mailer).await;
    let session = post(
        &addr,
        "/api/v1/login",
        r#"{"username":"old","password":"correct horse battery"}"#,
        None,
    )
    .await
    .cookie
    .unwrap();
    let me = request(&addr, "GET", "/api/v1/me", "", Some(&session)).await;
    assert!(me.body.contains(r#""email":null"#), "{}", me.body);
    let r = post(&addr, "/api/v1/account/email/send-link", "", Some(&session)).await;
    assert_eq!(r.status, 400);
    assert!(r.body.contains("no_email"));

    // The password is required, and an address can belong to one account.
    let r = post(
        &addr,
        "/api/v1/account/email",
        r#"{"email":"old@example.com","password":"wrong password"}"#,
        Some(&session),
    )
    .await;
    assert_eq!(r.status, 401);
    assert!(r.body.contains("bad_password"));
    let r = post(
        &addr,
        "/api/v1/account/email",
        r#"{"email":"MADE@example.com","password":"correct horse battery"}"#,
        Some(&session),
    )
    .await;
    assert_eq!(r.status, 409);
    assert!(r.body.contains("email_taken"));
    assert!(take_mail(&outbox, 0).await.is_empty());

    let r = post(
        &addr,
        "/api/v1/account/email",
        r#"{"email":"old@example.com","password":"correct horse battery"}"#,
        Some(&session),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains(r#""link_sent":true"#));
    let first = take_mail(&outbox, 1).await;
    assert_eq!(first[0].to, "old@example.com");

    // Sending again replaces the first link.
    let r = post(&addr, "/api/v1/account/email/send-link", "", Some(&session)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let second = take_mail(&outbox, 1).await;
    let stale = format!(r#"{{"token":"{}"}}"#, link_token(&first[0], "verify"));
    assert_eq!(
        post(&addr, "/api/v1/email/verify", &stale, None)
            .await
            .status,
        400
    );
    let fresh = format!(r#"{{"token":"{}"}}"#, link_token(&second[0], "verify"));
    assert_eq!(
        post(&addr, "/api/v1/email/verify", &fresh, None)
            .await
            .status,
        200
    );

    // Three emails an hour per account: the next is refused.
    post(
        &addr,
        "/api/v1/account/email",
        r#"{"email":"old2@example.com","password":"correct horse battery"}"#,
        Some(&session),
    )
    .await;
    let r = post(&addr, "/api/v1/account/email/send-link", "", Some(&session)).await;
    assert_eq!(r.status, 429, "{}", r.body);
}

#[tokio::test]
async fn forgotten_password() {
    let (mailer, outbox) = Mailer::memory();
    let addr = start_server(mailer).await;
    let old_session = post(
        &addr,
        "/api/v1/login",
        r#"{"username":"admin-made","password":"correct horse battery"}"#,
        None,
    )
    .await
    .cookie
    .unwrap();

    // The same answer for an unknown account, one without an address, and
    // a real one; only the real one gets an email.
    for account in ["nobody", "nobody@example.com", "old", "made@EXAMPLE.com"] {
        let r = post(
            &addr,
            "/api/v1/password/forgot",
            &format!(r#"{{"account":"{account}"}}"#),
            None,
        )
        .await;
        assert_eq!(r.status, 200, "{account}: {}", r.body);
        assert_eq!(r.body, r#"{"requested":true}"#);
    }
    let mail = take_mail(&outbox, 1).await;
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(mail[0].to, "made@example.com");
    assert!(mail[0].subject.contains("Reset"));
    let token = link_token(&mail[0], "reset");

    // A rejected password does not use up the link.
    for (password, why) in [
        ("short", "at least 10"),
        ("admin-made-forever", "must not contain the username"),
    ] {
        let r = post(
            &addr,
            "/api/v1/password/reset",
            &format!(r#"{{"token":"{token}","password":"{password}"}}"#),
            None,
        )
        .await;
        assert_eq!(r.status, 400);
        assert!(r.body.contains(why), "{}", r.body);
    }
    let r = post(
        &addr,
        "/api/v1/password/reset",
        &format!(r#"{{"token":"{token}","password":"a brand new password"}}"#),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains(r#""username":"admin-made""#));
    let new_session = r.cookie.expect("signed in");
    assert_eq!(
        request(&addr, "GET", "/api/v1/me", "", Some(&new_session))
            .await
            .status,
        200
    );
    // Older sessions are signed out, the old password is gone, and the link
    // worked once.
    assert_eq!(
        request(&addr, "GET", "/api/v1/me", "", Some(&old_session))
            .await
            .status,
        401
    );
    let r = post(
        &addr,
        "/api/v1/login",
        r#"{"username":"admin-made","password":"correct horse battery"}"#,
        None,
    )
    .await;
    assert_eq!(r.status, 401);
    let r = post(
        &addr,
        "/api/v1/password/reset",
        &format!(r#"{{"token":"{token}","password":"another new password"}}"#),
        None,
    )
    .await;
    assert_eq!(r.status, 400);
    assert!(r.body.contains("link_invalid"));
}

#[tokio::test]
async fn an_unconfirmed_address_gets_no_reset_link() {
    let (mailer, outbox) = Mailer::memory();
    let addr = start_server(mailer).await;
    post(
        &addr,
        "/api/v1/register",
        r#"{"username":"bob","password":"correct horse battery","email":"bob@example.com"}"#,
        None,
    )
    .await;
    take_mail(&outbox, 1).await;
    let r = post(
        &addr,
        "/api/v1/password/forgot",
        r#"{"account":"bob"}"#,
        None,
    )
    .await;
    assert_eq!(r.status, 200);
    assert!(take_mail(&outbox, 0).await.is_empty());
}

#[tokio::test]
async fn without_email_settings() {
    let addr = start_server(Mailer::Disabled).await;
    let info = request(&addr, "GET", "/api/v1/info", "", None).await;
    assert!(
        info.body.contains(r#""password_reset":false"#),
        "{}",
        info.body
    );
    // Accounts still need an address, kept for when email is set up.
    let r = post(
        &addr,
        "/api/v1/register",
        r#"{"username":"carol","password":"correct horse battery","email":"carol@example.com"}"#,
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = post(
        &addr,
        "/api/v1/password/forgot",
        r#"{"account":"carol"}"#,
        None,
    )
    .await;
    assert_eq!(r.status, 503);
    assert!(r.body.contains("cannot send email"));
}

#[tokio::test]
async fn reset_requests_are_limited_per_address() {
    let (mailer, _outbox) = Mailer::memory();
    let addr = start_server(mailer).await;
    for _ in 0..10 {
        let r = post(
            &addr,
            "/api/v1/password/forgot",
            r#"{"account":"nobody"}"#,
            None,
        )
        .await;
        assert_eq!(r.status, 200);
    }
    let r = post(
        &addr,
        "/api/v1/password/forgot",
        r#"{"account":"nobody"}"#,
        None,
    )
    .await;
    assert_eq!(r.status, 429, "{}", r.body);
}

#[tokio::test]
async fn waiting_addresses_block_no_one() {
    let (mailer, outbox) = Mailer::memory();
    let addr = start_server(mailer).await;
    let register = |name: &'static str, email: &'static str| {
        let addr = addr.clone();
        async move {
            post(
                &addr,
                "/api/v1/register",
                &format!(
                    r#"{{"username":"{name}","password":"correct horse battery","email":"{email}"}}"#
                ),
                None,
            )
            .await
        }
    };
    // Someone signs up with another person's address and never confirms;
    // the owner can still sign up with it.
    let squatter = register("squatter", "owner@example.com").await;
    assert_eq!(squatter.status, 200, "{}", squatter.body);
    let owner = register("owner", "Owner@example.com").await;
    assert_eq!(owner.status, 200, "{}", owner.body);
    let mail = take_mail(&outbox, 2).await;
    assert_eq!(mail.len(), 2);
    let squatter_link = format!(r#"{{"token":"{}"}}"#, link_token(&mail[0], "verify"));
    let owner_link = format!(r#"{{"token":"{}"}}"#, link_token(&mail[1], "verify"));

    // The owner confirms: the address is theirs, and the other account no
    // longer waits for it.
    let r = post(&addr, "/api/v1/email/verify", &owner_link, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = post(&addr, "/api/v1/email/verify", &squatter_link, None).await;
    assert_eq!(r.status, 400, "{}", r.body);
    let me = request(
        &addr,
        "GET",
        "/api/v1/me",
        "",
        Some(squatter.cookie.as_deref().unwrap()),
    )
    .await;
    assert!(
        me.body.contains(r#""email":null"#) && me.body.contains(r#""pending_email":null"#),
        "{}",
        me.body
    );
    // Now it is confirmed, it is taken.
    let r = register("third", "OWNER@example.com").await;
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("email_taken"));
}

#[tokio::test]
async fn changing_keeps_the_confirmed_address_until_the_new_one_is() {
    let (mailer, outbox) = Mailer::memory();
    let addr = start_server(mailer).await;
    let session = post(
        &addr,
        "/api/v1/login",
        r#"{"username":"admin-made","password":"correct horse battery"}"#,
        None,
    )
    .await
    .cookie
    .unwrap();
    // A reset link sent before the change.
    post(
        &addr,
        "/api/v1/password/forgot",
        r#"{"account":"admin-made"}"#,
        None,
    )
    .await;
    let early_reset = take_mail(&outbox, 1).await;

    let r = post(
        &addr,
        "/api/v1/account/email",
        r#"{"email":"fresh@example.com","password":"correct horse battery"}"#,
        Some(&session),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.body.contains(r#""email":"made@example.com""#)
            && r.body.contains(r#""pending_email":"fresh@example.com""#),
        "{}",
        r.body
    );
    let mail = take_mail(&outbox, 2).await;
    let verify = mail
        .iter()
        .find(|m| m.to == "fresh@example.com")
        .expect("link to the new address");
    let notice = mail
        .iter()
        .find(|m| m.to == "made@example.com")
        .expect("notice to the confirmed address");
    assert!(
        notice.subject.contains("being changed"),
        "{}",
        notice.subject
    );
    assert!(
        notice.body.contains("fresh@example.com")
            && notice.body.contains("still come to this address"),
        "{}",
        notice.body
    );

    // Meanwhile resets still go to the confirmed address.
    post(
        &addr,
        "/api/v1/password/forgot",
        r#"{"account":"admin-made"}"#,
        None,
    )
    .await;
    let mail = take_mail(&outbox, 1).await;
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].to, "made@example.com");

    // Confirming switches: the old address no longer gets reset links, and
    // links already sent there stop working.
    let body = format!(r#"{{"token":"{}"}}"#, link_token(verify, "verify"));
    assert_eq!(
        post(&addr, "/api/v1/email/verify", &body, None)
            .await
            .status,
        200
    );
    let me = request(&addr, "GET", "/api/v1/me", "", Some(&session)).await;
    assert!(
        me.body.contains(r#""email":"fresh@example.com""#)
            && me.body.contains(r#""pending_email":null"#),
        "{}",
        me.body
    );
    for old_link in [&early_reset[0], &mail[0]] {
        let r = post(
            &addr,
            "/api/v1/password/reset",
            &format!(
                r#"{{"token":"{}","password":"a brand new password"}}"#,
                link_token(old_link, "reset")
            ),
            None,
        )
        .await;
        assert_eq!(r.status, 400, "{}", r.body);
    }
    post(
        &addr,
        "/api/v1/password/forgot",
        r#"{"account":"made@example.com"}"#,
        None,
    )
    .await;
    assert!(take_mail(&outbox, 0).await.is_empty());

    // Choosing the confirmed address again cancels a change in progress.
    post(
        &addr,
        "/api/v1/account/email",
        r#"{"email":"typo@example.com","password":"correct horse battery"}"#,
        Some(&session),
    )
    .await;
    let r = post(
        &addr,
        "/api/v1/account/email",
        r#"{"email":"FRESH@example.com","password":"correct horse battery"}"#,
        Some(&session),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains(r#""pending_email":null"#), "{}", r.body);
}

/// A minimal SMTP server on this machine: accepts one message and returns
/// what the client sent after DATA.
async fn fake_smtp() -> (u16, tokio::sync::oneshot::Receiver<String>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    // The client's pool keeps the connection open, so report at the end of
    // the message rather than at QUIT.
    let mut tx = Some(tx);
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (r, mut w) = sock.into_split();
        let mut lines = BufReader::new(r).lines();
        w.write_all(b"220 test ESMTP\r\n").await.unwrap();
        let mut data = String::new();
        let mut in_data = false;
        while let Ok(Some(line)) = lines.next_line().await {
            if in_data {
                if line == "." {
                    in_data = false;
                    w.write_all(b"250 queued\r\n").await.unwrap();
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(std::mem::take(&mut data));
                    }
                } else {
                    data.push_str(&line);
                    data.push('\n');
                }
                continue;
            }
            let reply: &[u8] = match line.get(..4).unwrap_or("").to_ascii_uppercase().as_str() {
                "EHLO" => b"250 test\r\n",
                "DATA" => {
                    in_data = true;
                    b"354 go ahead\r\n"
                }
                "QUIT" => {
                    let _ = w.write_all(b"221 bye\r\n").await;
                    break;
                }
                _ => b"250 ok\r\n",
            };
            w.write_all(reply).await.unwrap();
        }
    });
    (port, rx)
}

#[tokio::test]
async fn sends_through_smtp() {
    let (port, received) = fake_smtp().await;
    let config = Config::parse(&format!(
        "public_url = \"https://audionet.example.com\"\n[email]\nfrom = \"AudioNet <no-reply@example.com>\"\nsmtp_host = \"localhost\"\nsmtp_port = {port}\nsmtp_security = \"none\""
    ))
    .unwrap();
    let mailer = Mailer::from_config(&config.email).unwrap();
    mailer
        .send(
            "someone@example.com",
            "Reset your AudioNet password",
            "Hello someone,\n\nhttps://audionet.example.com/?reset=anr_x\n".into(),
        )
        .await
        .unwrap();
    let data = tokio::time::timeout(Duration::from_secs(10), received)
        .await
        .unwrap()
        .unwrap();
    assert!(
        data.contains("From: AudioNet <no-reply@example.com>"),
        "{data}"
    );
    assert!(data.contains("To: someone@example.com"), "{data}");
    assert!(
        data.contains("Subject: Reset your AudioNet password"),
        "{data}"
    );
    assert!(data.contains("Content-Type: text/plain"), "{data}");
    assert!(
        data.contains("https://audionet.example.com/?reset=anr_x"),
        "{data}"
    );
}
