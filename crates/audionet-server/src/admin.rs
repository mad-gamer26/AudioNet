//! The admin page: accounts, devices, who is connected and what is
//! streaming, the settings and the log.
//!
//! It has its own listener (`[admin] bind`, 127.0.0.1:8741 by default),
//! never the public one. Without `[admin] password_hash` it must listen on
//! a loopback address; with one, browsers ask for the password (HTTP Basic,
//! any user name). Changes need the page's own header and an Origin that
//! matches the Host, so other sites can't make them through a browser.
//! Every change is written to the log with the address it came from.

use std::sync::{Mutex, OnceLock};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;

use crate::api::{ApiError, ClientAddress, Shared, platform_from};
use crate::auth::{self, hash_token, new_token};
use crate::db;
use crate::hub::NodeMeta;
use crate::mail::validate_email;

type AdminResult<T> = Result<T, ApiError>;

const PAGE: &str = include_str!("../admin/index.html");
const SCRIPT: &str = include_str!("../admin/app.js");
const STYLE: &str = include_str!("../admin/style.css");

static STARTED: OnceLock<i64> = OnceLock::new();

/// The admin page's routes, on the shared state.
pub fn router(state: Shared) -> Router {
    STARTED.get_or_init(db::now_s);
    Router::new()
        .route(
            "/",
            get(|| async { asset(PAGE, "text/html; charset=utf-8") }),
        )
        .route(
            "/app.js",
            get(|| async { asset(SCRIPT, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/style.css",
            get(|| async { asset(STYLE, "text/css; charset=utf-8") }),
        )
        .route("/api/overview", get(overview))
        .route("/api/users", get(users).post(create_user))
        .route("/api/users/{name}", get(user))
        .route("/api/users/{name}/password", post(set_password))
        .route("/api/users/{name}/email", post(set_email))
        .route("/api/users/{name}/delete", post(delete_user))
        .route("/api/users/{name}/link", post(make_link))
        .route(
            "/api/users/{name}/sign-out-browsers",
            post(sign_out_browsers),
        )
        .route(
            "/api/users/{name}/sessions/{id}/delete",
            post(delete_session),
        )
        .route("/api/devices/{id}/rename", post(rename_device))
        .route("/api/devices/{id}/delete", post(delete_device))
        .route("/api/connections/{id}/disconnect", post(disconnect))
        .route("/api/live", get(live))
        .route("/api/log", get(log))
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

fn asset(body: &'static str, kind: &'static str) -> Response {
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    r
}

fn err(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(status, code, message)
}

fn bad(message: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, "invalid", message)
}

fn not_found(what: &str) -> ApiError {
    err(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("{what} no longer exists. The page has been refreshed."),
    )
}

// ─── protection ─────────────────────────────────────────────────────────────

/// The last Authorization header that passed (as a hash), so the slow
/// password check runs once per browser, not on every request.
static GOOD_AUTH: Mutex<Option<auth::TokenHash>> = Mutex::new(None);

async fn guard(State(s): State<Shared>, req: Request, next: Next) -> Response {
    if let Some(hash) = s.config.admin.password_hash.clone()
        && !password_ok(req.headers(), &hash).await
    {
        let mut r = (StatusCode::UNAUTHORIZED, "The admin password is needed.").into_response();
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"AudioNet admin\", charset=\"UTF-8\""),
        );
        return r;
    }
    if req.method() != Method::GET && req.method() != Method::HEAD && !same_page(req.headers()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": {"code": "bad_origin", "message": "Requests from other sites are not allowed."}})),
        )
            .into_response();
    }
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
             form-action 'none'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    r
}

async fn password_ok(headers: &HeaderMap, hash: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let seen = hash_token(value);
    if GOOD_AUTH.lock().unwrap_or_else(|e| e.into_inner()).as_ref() == Some(&seen) {
        return true;
    }
    let Some(b64) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
        return false;
    };
    let Ok(text) = String::from_utf8(raw) else {
        return false;
    };
    let password = text
        .split_once(':')
        .map_or(text.as_str(), |(_, p)| p)
        .to_owned();
    let hash = hash.to_owned();
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash))
        .await
        .unwrap_or(false);
    if ok {
        *GOOD_AUTH.lock().unwrap_or_else(|e| e.into_inner()) = Some(seen);
    }
    ok
}

/// The page's own requests: its header, and an Origin naming this host.
fn same_page(h: &HeaderMap) -> bool {
    let header_ok = h.get("x-audionet-admin").and_then(|v| v.to_str().ok()) == Some("1");
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin = h.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let origin_host = origin
        .and_then(|o| o.split_once("://"))
        .map(|(_, rest)| rest);
    header_ok && host.is_some() && origin_host == host
}

fn audit(address: &str, what: impl std::fmt::Display) {
    tracing::info!(target: "audionet_server::admin", %address, "admin: {what}");
}

// ─── reading ────────────────────────────────────────────────────────────────

async fn overview(State(s): State<Shared>) -> AdminResult<Json<serde_json::Value>> {
    let (users, devices, browser_sessions) = s.db.call(db::admin_counts).await?;
    let live = s.hub.admin_snapshot();
    let c = &s.config;
    let hidden = |v: &Option<String>| {
        v.as_deref().map(|x| {
            if x.is_empty() {
                "(empty)"
            } else {
                "(set, hidden)"
            }
        })
    };
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "started": STARTED.get().copied(),
        "now": db::now_s(),
        "counts": {
            "users": users, "devices": devices, "browser_sessions": browser_sessions,
            "connections": live.connections.len(), "sessions": live.sessions.len(),
            "devices_connected": live.connections.iter().filter(|x| x.kind == "device").count(),
        },
        "email": s.mailer.enabled(),
        "push_gateway_here": s.apns.is_some(),
        "settings": [
            ["Public address", c.public_url],
            ["Listens on", c.bind.to_string()],
            ["Database", c.database.display().to_string()],
            ["Web client folder", c.web_root.as_ref().map(|p| p.display().to_string())],
            ["Downloads folder", c.downloads_dir.as_ref().map(|p| p.display().to_string())],
            ["Allowed page origins", c.allowed_origins.join(", ")],
            ["Anyone can create an account", c.allow_registration],
            ["Oldest app version accepted", c.min_client_version],
            ["Where people get a new app", c.client_download_url],
            ["New accounts per address per hour", c.sign_ups_per_address_per_hour],
            ["New accounts per hour", c.sign_ups_per_hour],
            ["Client address header", c.client_address_header],
            ["Browser sign-in lasts (days)", c.session_days],
            ["STUN servers", c.ice.stun_urls.join(", ")],
            ["TURN servers", c.ice.turn_urls.join(", ")],
            ["TURN secret", hidden(&c.ice.turn_secret)],
            ["TURN credentials last (seconds)", c.ice.turn_credential_ttl_s],
            ["Email from", c.email.from],
            ["Mail server", c.email.smtp_host.as_ref().map(|h| format!("{h}:{}", c.email.smtp_port.unwrap_or(587)))],
            ["Mail server user", c.email.smtp_username],
            ["Mail server password", hidden(&c.email.smtp_password)],
            ["Push notifications through", c.push_gateway_url],
            ["Push gateway on this server", s.apns.is_some()],
            ["Admin page listens on", c.admin.bind.to_string()],
            ["Admin page password", c.admin.password_hash.is_some()],
        ],
    })))
}

async fn users(State(s): State<Shared>) -> AdminResult<Json<serde_json::Value>> {
    let list = s.db.call(db::admin_users).await?;
    let live = s.hub.admin_snapshot();
    let rows: Vec<_> = list
        .into_iter()
        .map(|u| {
            let on = live
                .connections
                .iter()
                .filter(|c| c.user_id == u.id)
                .count();
            json!({ "user": u, "connected": on })
        })
        .collect();
    Ok(Json(json!({ "users": rows })))
}

async fn find_user(s: &Shared, name: &str) -> AdminResult<db::User> {
    let n = name.to_owned();
    s.db.call(move |c| db::user_by_name(c, &n))
        .await?
        .ok_or_else(|| not_found(&format!("The account {name}")))
}

async fn user(
    State(s): State<Shared>,
    Path(name): Path<String>,
) -> AdminResult<Json<serde_json::Value>> {
    let u = find_user(&s, &name).await?;
    let id = u.id;
    let (summary, devices, sessions, links) =
        s.db.call(move |c| {
            let summary = db::admin_users(c)?.into_iter().find(|x| x.id == id);
            Ok((
                summary,
                db::admin_devices(c, id)?,
                db::admin_web_sessions(c, id)?,
                db::sign_in_link_count(c, id)?,
            ))
        })
        .await?;
    let live = s.hub.admin_snapshot();
    let devices: Vec<_> = devices
        .into_iter()
        .map(|d| {
            let conn = live
                .connections
                .iter()
                .find(|c| c.node_id.as_deref() == Some(d.id.as_str()));
            json!({ "device": d, "connection": conn })
        })
        .collect();
    let browsers: Vec<_> = live
        .connections
        .iter()
        .filter(|c| c.user_id == id && c.kind == "browser")
        .collect();
    Ok(Json(json!({
        "user": summary,
        "devices": devices,
        "browsers": browsers,
        "web_sessions": sessions,
        "sign_in_links": links,
    })))
}

async fn live(State(s): State<Shared>) -> Json<serde_json::Value> {
    Json(json!(s.hub.admin_snapshot()))
}

#[derive(Deserialize)]
struct LogQuery {
    lines: Option<usize>,
}

async fn log(Query(q): Query<LogQuery>) -> Json<serde_json::Value> {
    let n = q.lines.unwrap_or(300).clamp(1, crate::logbuf::KEEP);
    Json(json!({ "lines": crate::logbuf::recent(n), "kept": crate::logbuf::KEEP }))
}

// ─── accounts ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct NewUser {
    username: String,
    password: String,
    email: Option<String>,
}

async fn hash(password: String) -> AdminResult<String> {
    tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, "internal", e))
}

/// An email address as the admin gives it: checked, free, recorded as
/// confirmed (the admin vouches for it, as with `audionet-server user`).
async fn clean_email(
    s: &Shared,
    email: Option<String>,
    except: i64,
) -> AdminResult<Option<String>> {
    let Some(e) = email.map(|e| e.trim().to_owned()).filter(|e| !e.is_empty()) else {
        return Ok(None);
    };
    let e = validate_email(&e).map_err(bad)?;
    let check = e.clone();
    if s.db
        .call(move |c| db::email_taken(c, &check, except))
        .await?
    {
        return Err(err(
            StatusCode::CONFLICT,
            "email_taken",
            format!("Another account already uses {e}."),
        ));
    }
    Ok(Some(e))
}

async fn create_user(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Json(r): Json<NewUser>,
) -> AdminResult<Json<serde_json::Value>> {
    let username = r.username.trim().to_owned();
    auth::validate_username(&username).map_err(bad)?;
    auth::validate_password(&r.password).map_err(bad)?;
    let email = clean_email(&s, r.email, 0).await?;
    let password_hash = hash(r.password).await?;
    let (name, mail) = (username.clone(), email.clone());
    let created =
        s.db.call(move |c| {
            if db::user_by_name(c, &name)?.is_some() {
                return Ok(false);
            }
            let id = db::create_user(c, &name, &password_hash)?;
            if let Some(e) = &mail {
                db::set_email(c, id, Some(e))?;
            }
            Ok(true)
        })
        .await?;
    if !created {
        return Err(err(
            StatusCode::CONFLICT,
            "username_taken",
            format!("There is already an account called {username}."),
        ));
    }
    audit(&address, format!("created the account {username}"));
    Ok(Json(
        json!({ "message": format!("Created the account {username}.") }),
    ))
}

#[derive(Deserialize)]
struct NewPassword {
    password: String,
}

async fn set_password(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path(name): Path<String>,
    Json(r): Json<NewPassword>,
) -> AdminResult<Json<serde_json::Value>> {
    let u = find_user(&s, &name).await?;
    auth::validate_password(&r.password).map_err(bad)?;
    let h = hash(r.password).await?;
    let id = u.id;
    s.db.call(move |c| db::set_password(c, id, &h)).await?;
    // A new password signs out browsers (their sign-ins are gone); devices
    // keep their own credentials.
    let n = s.hub.kick_user_browsers(id);
    audit(&address, format!("set a new password for {}", u.username));
    let extra = if n > 0 {
        format!(
            " {} open browser{} closed.",
            n,
            if n == 1 { " was" } else { "s were" }
        )
    } else {
        String::new()
    };
    Ok(Json(
        json!({ "message": format!("Set a new password for {}. Browsers are signed out; devices stay signed in.{extra}", u.username) }),
    ))
}

#[derive(Deserialize)]
struct NewEmail {
    email: Option<String>,
}

async fn set_email(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path(name): Path<String>,
    Json(r): Json<NewEmail>,
) -> AdminResult<Json<serde_json::Value>> {
    let u = find_user(&s, &name).await?;
    let email = clean_email(&s, r.email, u.id).await?;
    let (id, e) = (u.id, email.clone());
    s.db.call(move |c| db::set_email(c, id, e.as_deref()))
        .await?;
    let msg = match &email {
        Some(e) => format!("Set {}'s email address to {e} (confirmed).", u.username),
        None => format!("Removed {}'s email address.", u.username),
    };
    audit(&address, &msg);
    Ok(Json(json!({ "message": msg })))
}

async fn delete_user(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path(name): Path<String>,
) -> AdminResult<Json<serde_json::Value>> {
    let u = find_user(&s, &name).await?;
    let n = s.hub.kick_user(u.id);
    let username = u.username.clone();
    s.db.call(move |c| db::delete_user(c, &username)).await?;
    audit(
        &address,
        format!(
            "deleted the account {} ({n} connections closed)",
            u.username
        ),
    );
    Ok(Json(
        json!({ "message": format!("Deleted the account {} and its devices.{}", u.username,
        if n > 0 { format!(" {n} connection{} closed.", if n == 1 { " was" } else { "s were" }) } else { String::new() }) }),
    ))
}

#[derive(Deserialize)]
struct LinkRequest {
    hours: Option<u32>,
}

async fn make_link(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path(name): Path<String>,
    Json(r): Json<LinkRequest>,
) -> AdminResult<Json<serde_json::Value>> {
    let u = find_user(&s, &name).await?;
    let hours = r.hours.unwrap_or(24);
    if !(1..=168).contains(&hours) {
        return Err(bad("A link can work for 1 to 168 hours."));
    }
    let token = new_token("anl_");
    let h = hash_token(&token);
    let id = u.id;
    s.db.call(move |c| db::create_sign_in_link(c, &h, id, i64::from(hours) * 3600))
        .await?;
    let link = format!(
        "{}/?signin={token}",
        s.config.public_url.trim_end_matches('/')
    );
    audit(
        &address,
        format!("made a sign-in link for {} ({hours} hours)", u.username),
    );
    Ok(Json(json!({ "link": link, "hours": hours,
        "message": format!("Made a sign-in link for {}. It signs one web browser in, once, within {hours} hour{}.", u.username, if hours == 1 { "" } else { "s" }) })))
}

async fn sign_out_browsers(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path(name): Path<String>,
) -> AdminResult<Json<serde_json::Value>> {
    let u = find_user(&s, &name).await?;
    let id = u.id;
    let (sessions, links) =
        s.db.call(move |c| {
            Ok((
                db::delete_web_sessions_for_user(c, id)?,
                db::delete_sign_in_links(c, id)?,
            ))
        })
        .await?;
    let n = s.hub.kick_user_browsers(id);
    audit(
        &address,
        format!("signed out every browser of {}", u.username),
    );
    Ok(Json(json!({ "message": format!(
        "Signed out every browser of {}: {sessions} sign-in{} ended, {n} open browser{} closed{}.",
        u.username, if sessions == 1 { "" } else { "s" }, if n == 1 { "" } else { "s" },
        if links > 0 { format!(", {links} unused sign-in link{} cancelled", if links == 1 { "" } else { "s" }) } else { String::new() }) })))
}

async fn delete_session(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path((name, sid)): Path<(String, String)>,
) -> AdminResult<Json<serde_json::Value>> {
    let u = find_user(&s, &name).await?;
    let id = u.id;
    let gone =
        s.db.call(move |c| db::delete_web_session_by_id(c, id, &sid))
            .await?;
    if !gone {
        return Err(not_found("That browser sign-in"));
    }
    // Open browsers can't be told apart by sign-in, so all of the
    // account's close; those still signed in reconnect by themselves.
    s.hub.kick_user_browsers(id);
    audit(
        &address,
        format!("ended a browser sign-in of {}", u.username),
    );
    Ok(Json(
        json!({ "message": format!("Ended that browser sign-in of {}. Its other browsers reconnect by themselves.", u.username) }),
    ))
}

// ─── devices and connections ────────────────────────────────────────────────

#[derive(Deserialize)]
struct Rename {
    name: String,
}

async fn device_owner(s: &Shared, id: &str) -> AdminResult<(i64, String, String)> {
    let node = id.to_owned();
    s.db.call(move |c| db::node_owner(c, &node))
        .await?
        .ok_or_else(|| not_found("That device"))
}

async fn rename_device(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path(id): Path<String>,
    Json(r): Json<Rename>,
) -> AdminResult<Json<serde_json::Value>> {
    let (user_id, username, old) = device_owner(&s, &id).await?;
    let name = r.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(bad("A device name must be 1 to 100 characters long."));
    }
    let (node, n) = (id.clone(), name.clone());
    s.db.call(move |c| db::rename_node(c, &node, user_id, &n))
        .await?;
    let rows = s.db.call(move |c| db::nodes_for_user(c, user_id)).await?;
    if let Some(r) = rows.into_iter().find(|r| r.id == id) {
        // Open apps and browsers show the new name at once.
        s.hub.node_renamed(
            user_id,
            NodeMeta {
                platform: platform_from(r.platform.as_deref()),
                id: r.id,
                name: r.name,
            },
        );
    }
    audit(
        &address,
        format!("renamed {username}'s device {old} to {name}"),
    );
    Ok(Json(
        json!({ "message": format!("Renamed {username}'s device {old} to {name}.") }),
    ))
}

async fn delete_device(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path(id): Path<String>,
) -> AdminResult<Json<serde_json::Value>> {
    let (user_id, username, name) = device_owner(&s, &id).await?;
    let node = id.clone();
    s.db.call(move |c| db::delete_node(c, &node, user_id))
        .await?;
    // Its credential is gone, so it can't reconnect; its streams end.
    s.hub.kick_node(&id);
    audit(&address, format!("removed {username}'s device {name}"));
    Ok(Json(
        json!({ "message": format!("Removed {username}'s device {name}. It is signed out and disconnected.") }),
    ))
}

async fn disconnect(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Path(id): Path<String>,
) -> AdminResult<Json<serde_json::Value>> {
    let snap = s.hub.admin_snapshot();
    let Some(c) = snap.connections.iter().find(|c| c.conn_id == id) else {
        return Err(not_found("That connection"));
    };
    let what = match &c.node_name {
        Some(n) => format!("{}'s device {n}", c.username),
        None => format!("a browser of {}", c.username),
    };
    s.hub.kick_conn(&id);
    audit(&address, format!("disconnected {what}"));
    Ok(Json(
        json!({ "message": format!("Disconnected {what}. It is still signed in, so it reconnects by itself.") }),
    ))
}
