//! HTTP API and WebSocket signaling endpoint.
//!
//! Authentication:
//! * Browsers: `audionet_session` cookie (HttpOnly, SameSite=Strict, Secure
//!   on HTTPS). Cookie-authenticated requests must carry an allowed `Origin`
//!   (browsers always send it on POST, DELETE and WebSocket upgrades).
//! * Devices: `Authorization: Bearer ann_…` device token, issued when the
//!   device signs in with the account password.
//!
//! Email: new accounts give an address (older ones may add one); the server
//! emails a link to confirm it, and password-reset links go only to
//! confirmed addresses.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use audionet_protocol::signal::{ClientKind, ClientMessage, IceServer, ServerMessage};
use audionet_protocol::{NodeId, PROTOCOL_VERSION, Platform};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use crate::auth::{self, HourlyLimit, HourlyWait, Throttle, hash_token, new_token};
use crate::config::Config;
use crate::db::{self, Db, EmailPurpose};
use crate::hub::{Hub, NodeMeta, QUEUE_DEPTH};
use crate::mail::{Mailer, validate_email};

pub const COOKIE: &str = "audionet_session";
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const WS_PING: Duration = Duration::from_secs(20);
const MAX_WS_MESSAGE: usize = 64 * 1024;
/// How long a link confirming an email address works.
const VERIFY_LINK_TTL_S: i64 = 7 * 86_400;
/// How long a password-reset link works.
const RESET_LINK_TTL_S: i64 = 3600;
/// Emails one account may be sent per hour, and the whole server.
const EMAILS_PER_ACCOUNT_PER_HOUR: u32 = 3;
const EMAILS_PER_HOUR: u32 = 100;
/// Password-reset requests one client address may make per hour.
const RESET_REQUESTS_PER_ADDRESS_PER_HOUR: u32 = 10;

#[derive(Debug)]
pub struct AppState {
    pub config: Config,
    pub db: Db,
    pub hub: Hub,
    pub login_throttle: Throttle,
    pub sign_up_limit: HourlyLimit,
    pub mailer: Mailer,
    /// Emails sent per account.
    pub email_limit: HourlyLimit,
    /// Password-reset requests per client address.
    pub reset_request_limit: HourlyLimit,
}

impl AppState {
    pub fn new(config: Config, db: Db, mailer: Mailer) -> Self {
        Self {
            config,
            db,
            hub: Hub::default(),
            login_throttle: Throttle::default(),
            sign_up_limit: HourlyLimit::default(),
            mailer,
            email_limit: HourlyLimit::default(),
            reset_request_limit: HourlyLimit::default(),
        }
    }
}

pub type Shared = Arc<AppState>;

// ─── errors ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {e}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "The server hit an internal error. Details are in the server log.",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        Self::internal(e)
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ─── authentication ─────────────────────────────────────────────────────────

/// An authenticated caller.
#[derive(Clone, Debug)]
pub struct Principal {
    pub user_id: i64,
    pub username: String,
    /// Set for device (node token) callers.
    pub node: Option<NodeMeta>,
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn check_origin(state: &AppState, parts: &Parts) -> Result<(), ApiError> {
    let origin = parts
        .headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    match origin {
        Some(o)
            if state
                .config
                .allowed_origins
                .iter()
                .any(|a| a.eq_ignore_ascii_case(o)) =>
        {
            Ok(())
        }
        Some(_) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "bad_origin",
            "This page is not allowed to use this server.",
        )),
        // Safe methods without Origin are fine (same-origin GET navigations).
        None if parts.method == Method::GET && !is_upgrade(parts) => Ok(()),
        None => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "bad_origin",
            "Missing Origin header.",
        )),
    }
}

fn is_upgrade(parts: &Parts) -> bool {
    parts.headers.contains_key(header::UPGRADE)
}

fn platform_from(s: Option<&str>) -> Option<Platform> {
    s.and_then(|p| serde_json::from_value(serde_json::Value::String(p.to_owned())).ok())
}

impl FromRequestParts<Shared> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Shared,
    ) -> Result<Self, Self::Rejection> {
        let unauthorized =
            || ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "Please sign in.");
        let bearer = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned);
        if let Some(token) = bearer {
            let hash = hash_token(&token);
            if token.starts_with("ann_") {
                let found = state.db.call(move |c| db::node_by_token(c, &hash)).await?;
                let (node, username) = found.ok_or_else(unauthorized)?;
                return Ok(Principal {
                    user_id: node.user_id,
                    username,
                    node: Some(NodeMeta {
                        id: node.id,
                        name: node.name,
                        platform: platform_from(node.platform.as_deref()),
                    }),
                });
            }
            let found = state
                .db
                .call(move |c| db::web_session_user(c, &hash))
                .await?;
            let (user_id, username) = found.ok_or_else(unauthorized)?;
            return Ok(Principal {
                user_id,
                username,
                node: None,
            });
        }
        let token = cookie_value(&parts.headers, COOKIE)
            .ok_or_else(unauthorized)?
            .to_owned();
        check_origin(state, parts)?;
        let hash = hash_token(&token);
        let found = state
            .db
            .call(move |c| db::web_session_user(c, &hash))
            .await?;
        let (user_id, username) = found.ok_or_else(unauthorized)?;
        Ok(Principal {
            user_id,
            username,
            node: None,
        })
    }
}

/// Origin check for unauthenticated state-changing browser requests (login).
struct BrowserOrigin;

impl FromRequestParts<Shared> for BrowserOrigin {
    type Rejection = ApiError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &Shared,
    ) -> Result<Self, Self::Rejection> {
        // Requests without cookies and without Origin come from non-browser
        // clients (the CLI), which cannot be CSRF-ed.
        if parts.headers.contains_key(header::ORIGIN) {
            check_origin(state, parts)?;
        }
        Ok(BrowserOrigin)
    }
}

/// The client's address, for the sign-up limit: from the header the reverse
/// proxy sets (`client_address_header`), else the connection's own.
struct ClientAddress(String);

impl FromRequestParts<Shared> for ClientAddress {
    type Rejection = ApiError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &Shared,
    ) -> Result<Self, Self::Rejection> {
        let from_proxy = state
            .config
            .client_address_header
            .as_deref()
            .and_then(|h| parts.headers.get(h))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        let direct = || {
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip().to_string())
        };
        Ok(ClientAddress(
            from_proxy
                .or_else(direct)
                .unwrap_or_else(|| "unknown".into()),
        ))
    }
}

fn session_cookie(state: &AppState, token: &str, max_age_s: i64) -> HeaderValue {
    let secure = if state.config.secure_cookies() {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_s}{secure}"
    ))
    .expect("cookie is ASCII")
}

// ─── routes ─────────────────────────────────────────────────────────────────

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/info", get(info))
        .route("/api/v1/register", post(register))
        .route("/api/v1/login", post(login))
        .route("/api/v1/logout", post(logout))
        .route("/api/v1/me", get(me))
        .route("/api/v1/account/email", post(change_email))
        .route("/api/v1/account/email/send-link", post(send_verify_link))
        .route("/api/v1/email/verify", post(verify_email))
        .route("/api/v1/password/forgot", post(forgot_password))
        .route("/api/v1/password/reset", post(reset_password))
        .route("/api/v1/nodes/sign-in", post(sign_in_node))
        .route("/api/v1/nodes", get(list_nodes))
        .route("/api/v1/nodes/{id}", delete(remove_node).patch(rename_node))
        .route("/api/v1/ws", get(ws_upgrade))
        .with_state(state)
}

async fn health(State(s): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "connections": s.hub.connection_count() }))
}

async fn info(State(s): State<Shared>) -> Json<serde_json::Value> {
    let downloads = s.config.downloads_dir.as_ref().map(|_| "/downloads/");
    Json(json!({
        "name": "AudioNet",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": PROTOCOL_VERSION,
        "allow_registration": s.config.allow_registration,
        "email_required": true,
        "password_reset": s.mailer.enabled(),
        "downloads_path": downloads,
    }))
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct NewAccount {
    username: String,
    password: String,
    /// Required; `Option` only so a missing field gets a clear message.
    #[serde(default)]
    email: Option<String>,
}

#[derive(Serialize)]
struct SignedIn {
    username: String,
    /// Returned for non-browser clients; browsers use the cookie.
    token: String,
}

async fn register(
    State(s): State<Shared>,
    _o: BrowserOrigin,
    ClientAddress(address): ClientAddress,
    Json(c): Json<NewAccount>,
) -> ApiResult<Response> {
    let username = create_account(&s, &address, &c).await?;
    sign_in(&s, username).await
}

fn bad_email(m: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_email", m)
}

fn email_taken() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "email_taken",
        "Another account already uses that email address.",
    )
}

/// Minutes, rounded up, for a message.
fn minutes(d: Duration) -> u64 {
    d.as_secs().div_ceil(60).max(1)
}

/// Creates an account someone asked for themselves (in the web client;
/// the apps only sign in): only when the server allows it, within the sign-up
/// limits, and with the self-service name and password rules. Returns the
/// username.
async fn create_account(s: &Shared, address: &str, c: &NewAccount) -> ApiResult<String> {
    if !s.config.allow_registration {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "registration_closed",
            "This server does not allow new accounts. Ask its administrator.",
        ));
    }
    auth::validate_username(&c.username)
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, "bad_username", m))?;
    auth::validate_password(&c.password)
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, "bad_password", m))?;
    auth::validate_new_account(&c.username, &c.password)
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, "bad_new_account", m))?;
    let email = validate_email(c.email.as_deref().unwrap_or_default()).map_err(bad_email)?;
    let limits = &s.config;
    match s.sign_up_limit.check(
        address,
        limits.sign_ups_per_address_per_hour,
        limits.sign_ups_per_hour,
        Instant::now(),
    ) {
        Ok(()) => {}
        Err(HourlyWait::Key(d)) => {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "sign_up_limit",
                format!(
                    "Too many new accounts were created from your network recently. Try again in {} minutes.",
                    minutes(d)
                ),
            ));
        }
        Err(HourlyWait::Total(d)) => {
            tracing::warn!("sign-up limit for the whole server reached");
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "sign_up_limit",
                format!(
                    "This server has had many new accounts in the last hour. Try again in {} minutes.",
                    minutes(d)
                ),
            ));
        }
    }
    let password = c.password.clone();
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    let name = c.username.clone();
    let address_for_db = email.clone();
    let created =
        s.db.call(move |conn| {
            if db::user_by_name(conn, &name)?.is_some() {
                return Ok(Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "username_taken",
                    "That username is taken. Choose another.",
                )));
            }
            if db::email_taken(conn, &address_for_db, 0)? {
                return Ok(Err(email_taken()));
            }
            let id = db::create_user(conn, &name, &hash)?;
            db::set_email(conn, id, Some(&address_for_db), false)?;
            Ok(Ok(id))
        })
        .await;
    let user_id = match created {
        Ok(Ok(id)) => id,
        Ok(Err(e)) => return Err(e),
        // Lost a race with another sign-up for the same name or address.
        Err(_) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "username_taken",
                "That username or email address is taken. Choose another.",
            ));
        }
    };
    s.sign_up_limit.record(address, Instant::now());
    tracing::info!(user = %c.username, "account created");
    send_link(s, user_id, &c.username, &email, EmailPurpose::Verify).await?;
    Ok(c.username.clone())
}

/// The server's host name, as emails name it.
fn server_host(s: &AppState) -> String {
    crate::config::origin_of(&s.config.public_url)
        .and_then(|o| o.split_once("://").map(|(_, h)| h.to_owned()))
        .unwrap_or_else(|| s.config.public_url.clone())
}

/// Emails `user` a link of the given kind, if the server can send email and
/// the account has not had too many emails this hour. Returns whether it was
/// sent.
async fn send_link(
    s: &Shared,
    user_id: i64,
    username: &str,
    email: &str,
    purpose: EmailPurpose,
) -> ApiResult<bool> {
    if !s.mailer.enabled() {
        return Ok(false);
    }
    let key = format!("user:{user_id}");
    if s.email_limit
        .check(
            &key,
            EMAILS_PER_ACCOUNT_PER_HOUR,
            EMAILS_PER_HOUR,
            Instant::now(),
        )
        .is_err()
    {
        tracing::warn!(user = %username, "email limit reached; not sending");
        return Ok(false);
    }
    let (prefix, ttl, param) = match purpose {
        EmailPurpose::Verify => ("anv_", VERIFY_LINK_TTL_S, "verify"),
        EmailPurpose::Reset => ("anr_", RESET_LINK_TTL_S, "reset"),
    };
    let token = new_token(prefix);
    let hash = hash_token(&token);
    let to = email.to_owned();
    s.db.call(move |c| db::create_email_token(c, &hash, user_id, purpose, &to, ttl))
        .await?;
    s.email_limit.record(&key, Instant::now());
    let link = format!(
        "{}/?{param}={token}",
        s.config.public_url.trim_end_matches('/')
    );
    let host = server_host(s);
    let (subject, body) = match purpose {
        EmailPurpose::Verify => (
            "Confirm your email address for AudioNet",
            format!(
                "Hello {username},\n\n\
                 Confirm that this is the email address for your AudioNet account on {host} by opening this link:\n\n\
                 {link}\n\n\
                 The link works for 7 days. AudioNet uses this address only to reset your password if you forget it.\n\n\
                 If you did not create an AudioNet account or add this address, you can ignore this email.\n"
            ),
        ),
        EmailPurpose::Reset => (
            "Reset your AudioNet password",
            format!(
                "Hello {username},\n\n\
                 Someone, hopefully you, asked to reset the password for your AudioNet account on {host}. To choose a new password, open this link:\n\n\
                 {link}\n\n\
                 The link works for 1 hour, once. If you did not ask for this, ignore this email; your password stays the same.\n"
            ),
        ),
    };
    s.mailer.send_later(email.to_owned(), subject, body);
    Ok(true)
}

async fn sign_in(s: &Shared, username: String) -> ApiResult<Response> {
    let token = new_token("ans_");
    let hash = hash_token(&token);
    let ttl = i64::from(s.config.session_days) * 86_400;
    let name = username.clone();
    s.db.call(move |c| {
        let user = db::user_by_name(c, &name)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        db::create_web_session(c, &hash, user.id, ttl)
    })
    .await?;
    let mut resp = Json(SignedIn {
        username,
        token: token.clone(),
    })
    .into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, session_cookie(s, &token, ttl));
    Ok(resp)
}

async fn login(
    State(s): State<Shared>,
    _o: BrowserOrigin,
    Json(c): Json<Credentials>,
) -> ApiResult<Response> {
    let username = check_credentials(&s, c).await?;
    sign_in(&s, username).await
}

/// Verifies a username and password (throttled per username, constant
/// work for unknown names). Returns the canonical username.
async fn check_credentials(s: &Shared, c: Credentials) -> ApiResult<String> {
    let key = c.username.to_lowercase();
    if let Some(wait) = s.login_throttle.wait_time(&key) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "throttled",
            format!(
                "Too many failed sign-in attempts. Try again in {} seconds.",
                wait.as_secs() + 1
            ),
        ));
    }
    let name = c.username.clone();
    let user = s.db.call(move |conn| db::user_by_name(conn, &name)).await?;
    let password = c.password;
    let (ok, username) = tokio::task::spawn_blocking(move || match user {
        Some(u) => (
            auth::verify_password(&password, &u.password_hash),
            u.username,
        ),
        None => {
            let _ = auth::verify_password(&password, auth::dummy_hash());
            (false, String::new())
        }
    })
    .await
    .map_err(ApiError::internal)?;
    if !ok {
        s.login_throttle.failure(&key);
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "bad_credentials",
            "The username or password is incorrect.",
        ));
    }
    s.login_throttle.success(&key);
    Ok(username)
}

#[derive(Deserialize)]
struct DeviceSignIn {
    username: String,
    password: String,
    /// Name for this device in the account, e.g. "Matthew's iPhone".
    name: String,
    platform: Option<Platform>,
}

/// Devices sign in with the account password and register themselves,
/// receiving their own device token (the password is not stored on the
/// device). This is the only way to add a device to an account.
async fn sign_in_node(
    State(s): State<Shared>,
    _o: BrowserOrigin,
    Json(r): Json<DeviceSignIn>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = device_name(&r.name)?;
    let username = check_credentials(
        &s,
        Credentials {
            username: r.username,
            password: r.password,
        },
    )
    .await?;
    add_device(&s, username, name, r.platform).await
}

fn device_name(name: &str) -> ApiResult<String> {
    let name = name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_name",
            "A device name must be 1 to 100 characters long.",
        ));
    }
    Ok(name)
}

/// Adds a device to `username`'s account and issues its token.
async fn add_device(
    s: &Shared,
    username: String,
    name: String,
    platform: Option<Platform>,
) -> ApiResult<Json<serde_json::Value>> {
    let token = new_token("ann_");
    let token_hash = hash_token(&token);
    let node_id = format!("node_{}", &new_token("")[..16]);
    let platform = platform
        .and_then(|p| serde_json::to_value(p).ok())
        .and_then(|v| v.as_str().map(str::to_owned));
    let (id, user) = (node_id.clone(), username.clone());
    s.db.call(move |c| {
        let u = db::user_by_name(c, &user)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        db::create_node(c, &id, u.id, &name, platform.as_deref(), &token_hash)
    })
    .await?;
    tracing::info!(node = %node_id, user = %username, "device signed in");
    Ok(Json(
        json!({ "node_id": node_id, "token": token, "username": username }),
    ))
}

async fn logout(State(s): State<Shared>, headers: HeaderMap, _p: Principal) -> ApiResult<Response> {
    if let Some(token) = cookie_value(&headers, COOKIE) {
        let hash = hash_token(token);
        s.db.call(move |c| db::delete_web_session(c, &hash)).await?;
    }
    let mut resp = Json(json!({ "signed_out": true })).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&s, "", 0));
    Ok(resp)
}

async fn me(State(s): State<Shared>, p: Principal) -> ApiResult<Json<serde_json::Value>> {
    let uid = p.user_id;
    let user =
        s.db.call(move |c| db::user_by_id(c, uid))
            .await?
            .ok_or_else(|| {
                ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "Please sign in.")
            })?;
    Ok(Json(json!({
        "username": p.username,
        "node_id": p.node.map(|n| n.id),
        "email": user.email,
        "email_verified": user.email_verified,
    })))
}

#[derive(Deserialize)]
struct ChangeEmail {
    email: String,
    /// The account password, so an unattended signed-in browser cannot
    /// redirect password resets.
    password: String,
}

/// Sets the account's email address (unconfirmed) and emails a link to
/// confirm it.
async fn change_email(
    State(s): State<Shared>,
    p: Principal,
    Json(r): Json<ChangeEmail>,
) -> ApiResult<Json<serde_json::Value>> {
    let email = validate_email(&r.email).map_err(bad_email)?;
    check_credentials(
        &s,
        Credentials {
            username: p.username.clone(),
            password: r.password,
        },
    )
    .await
    .map_err(|e| {
        if e.code == "bad_credentials" {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "bad_password",
                "The password is incorrect.",
            )
        } else {
            e
        }
    })?;
    let uid = p.user_id;
    let address = email.clone();
    let taken =
        s.db.call(move |c| {
            if db::email_taken(c, &address, uid)? {
                return Ok(true);
            }
            db::set_email(c, uid, Some(&address), false)?;
            Ok(false)
        })
        .await
        .map_err(|_| email_taken())?;
    if taken {
        return Err(email_taken());
    }
    tracing::info!(user = %p.username, "email address changed");
    let sent = send_link(&s, uid, &p.username, &email, EmailPurpose::Verify).await?;
    Ok(Json(json!({
        "email": email,
        "email_verified": false,
        "link_sent": sent,
    })))
}

/// Emails the account's address another confirmation link.
async fn send_verify_link(
    State(s): State<Shared>,
    p: Principal,
) -> ApiResult<Json<serde_json::Value>> {
    let uid = p.user_id;
    let user =
        s.db.call(move |c| db::user_by_id(c, uid))
            .await?
            .ok_or_else(|| {
                ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "Please sign in.")
            })?;
    let Some(email) = user.email else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "no_email",
            "This account has no email address. Add one first.",
        ));
    };
    if user.email_verified {
        return Ok(Json(json!({ "email_verified": true, "link_sent": false })));
    }
    if !s.mailer.enabled() {
        return Err(no_email_service());
    }
    if !send_link(&s, uid, &p.username, &email, EmailPurpose::Verify).await? {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "email_limit",
            "Several emails were sent to this account in the last hour. Wait an hour, then try again.",
        ));
    }
    Ok(Json(json!({ "email_verified": false, "link_sent": true })))
}

fn no_email_service() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "email_unavailable",
        "This server cannot send email. Ask its administrator.",
    )
}

fn link_invalid() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "link_invalid",
        "This link has expired or was already used. Ask for a new one.",
    )
}

#[derive(Deserialize)]
struct LinkToken {
    token: String,
}

/// Confirms an email address from the emailed link.
async fn verify_email(
    State(s): State<Shared>,
    _o: BrowserOrigin,
    Json(r): Json<LinkToken>,
) -> ApiResult<Json<serde_json::Value>> {
    let hash = hash_token(&r.token);
    let user =
        s.db.call(move |c| {
            let user = db::take_email_token(c, &hash, EmailPurpose::Verify)?;
            if let Some(u) = &user {
                db::mark_email_verified(c, u.id)?;
            }
            Ok(user)
        })
        .await?
        .ok_or_else(link_invalid)?;
    tracing::info!(user = %user.username, "email address confirmed");
    Ok(Json(json!({
        "username": user.username,
        "email": user.email,
        "email_verified": true,
    })))
}

#[derive(Deserialize)]
struct Forgot {
    /// Username or email address.
    account: String,
}

/// Emails a password-reset link to the account's confirmed address. The
/// answer is the same whether or not the account exists or has one.
async fn forgot_password(
    State(s): State<Shared>,
    _o: BrowserOrigin,
    ClientAddress(address): ClientAddress,
    Json(r): Json<Forgot>,
) -> ApiResult<Json<serde_json::Value>> {
    if !s.mailer.enabled() {
        return Err(no_email_service());
    }
    let account = r.account.trim().to_owned();
    if account.is_empty() || account.len() > 254 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_account",
            "Enter your username or email address.",
        ));
    }
    if let Err(HourlyWait::Key(d) | HourlyWait::Total(d)) = s.reset_request_limit.check(
        &address,
        RESET_REQUESTS_PER_ADDRESS_PER_HOUR,
        u32::MAX,
        Instant::now(),
    ) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "reset_limit",
            format!(
                "Too many password resets were requested from your network. Try again in {} minutes.",
                minutes(d)
            ),
        ));
    }
    s.reset_request_limit.record(&address, Instant::now());
    let user =
        s.db.call(move |c| {
            if account.contains('@') {
                db::user_by_email(c, &account)
            } else {
                db::user_by_name(c, &account)
            }
        })
        .await?;
    match user {
        Some(db::User {
            id,
            username,
            email: Some(email),
            email_verified: true,
            ..
        }) => {
            if send_link(&s, id, &username, &email, EmailPurpose::Reset).await? {
                tracing::info!(user = %username, "password reset link sent");
            }
        }
        Some(u) => {
            tracing::info!(user = %u.username, "password reset asked for an account without a confirmed email address")
        }
        None => {}
    }
    Ok(Json(json!({ "requested": true })))
}

#[derive(Deserialize)]
struct ResetPassword {
    token: String,
    password: String,
}

/// Sets a new password from an emailed link, signs out the account's web
/// sessions, and signs this browser in.
async fn reset_password(
    State(s): State<Shared>,
    _o: BrowserOrigin,
    Json(r): Json<ResetPassword>,
) -> ApiResult<Response> {
    auth::validate_password(&r.password)
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, "bad_password", m))?;
    // Check the password against the name before using up the link, so a
    // rejected password does not cost the link.
    let hash = hash_token(&r.token);
    let peek = hash.clone();
    let owner =
        s.db.call(move |c| db::email_token_owner(c, &peek, EmailPurpose::Reset))
            .await?
            .ok_or_else(link_invalid)?;
    if r.password.to_lowercase().contains(&owner.to_lowercase()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_password",
            "The password must not contain the username.",
        ));
    }
    let password = r.password;
    let new_hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    let user =
        s.db.call(move |c| {
            let user = db::take_email_token(c, &hash, EmailPurpose::Reset)?;
            if let Some(u) = &user {
                db::set_password(c, u.id, &new_hash)?;
                // The link reached the address, so it is confirmed.
                db::mark_email_verified(c, u.id)?;
            }
            Ok(user)
        })
        .await?
        .ok_or_else(link_invalid)?;
    s.login_throttle.success(&user.username.to_lowercase());
    tracing::info!(user = %user.username, "password reset");
    sign_in(&s, user.username).await
}

async fn list_nodes(State(s): State<Shared>, p: Principal) -> ApiResult<Json<serde_json::Value>> {
    let rows = s.db.call(move |c| db::nodes_for_user(c, p.user_id)).await?;
    let metas: Vec<NodeMeta> = rows
        .into_iter()
        .map(|r| NodeMeta {
            platform: platform_from(r.platform.as_deref()),
            id: r.id,
            name: r.name,
        })
        .collect();
    Ok(Json(json!({ "nodes": s.hub.summaries(&metas) })))
}

async fn remove_node(
    State(s): State<Shared>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let node = id.clone();
    let removed =
        s.db.call(move |c| db::delete_node(c, &node, p.user_id))
            .await?;
    if !removed {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "No such device.",
        ));
    }
    s.hub.kick_node(&id);
    Ok(Json(json!({ "removed": id })))
}

#[derive(Deserialize)]
struct Rename {
    name: String,
}

async fn rename_node(
    State(s): State<Shared>,
    p: Principal,
    Path(id): Path<String>,
    Json(r): Json<Rename>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = r.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_name",
            "A device name must be 1 to 100 characters long.",
        ));
    }
    let node = id.clone();
    let changed =
        s.db.call(move |c| db::rename_node(c, &node, p.user_id, &name))
            .await?;
    if !changed {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "No such device.",
        ));
    }
    // Open browsers and apps show the new name at once.
    let user_id = p.user_id;
    let rows = s.db.call(move |c| db::nodes_for_user(c, user_id)).await?;
    if let Some(r) = rows.into_iter().find(|r| r.id == id) {
        s.hub.node_renamed(
            user_id,
            NodeMeta {
                platform: platform_from(r.platform.as_deref()),
                id: r.id,
                name: r.name,
            },
        );
    }
    Ok(Json(json!({ "renamed": id })))
}

// ─── WebSocket signaling ────────────────────────────────────────────────────

async fn ws_upgrade(State(s): State<Shared>, p: Principal, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_WS_MESSAGE)
        .on_upgrade(move |socket| run_connection(s, p, socket))
}

fn ice_servers(s: &AppState, p: &Principal) -> Vec<IceServer> {
    let ice = &s.config.ice;
    let mut out = Vec::new();
    if !ice.stun_urls.is_empty() {
        out.push(IceServer {
            urls: ice.stun_urls.clone(),
            username: None,
            credential: None,
        });
    }
    if let (false, Some(secret)) = (ice.turn_urls.is_empty(), ice.turn_secret.as_deref()) {
        let (username, credential) = auth::turn_credentials(
            secret,
            &p.username,
            ice.turn_credential_ttl_s,
            db::now_s() as u64,
        );
        out.push(IceServer {
            urls: ice.turn_urls.clone(),
            username: Some(username),
            credential: Some(credential),
        });
    }
    out
}

fn already_over(e: crate::hub::HubError) -> Result<(), crate::hub::HubError> {
    if e.code == "no_session" {
        Ok(())
    } else {
        Err(e)
    }
}

fn error_msg(code: &str, message: impl Into<String>) -> ServerMessage {
    ServerMessage::Error {
        code: code.into(),
        message: message.into(),
        session_id: None,
    }
}

async fn run_connection(s: Shared, p: Principal, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();

    // First message must be `hello`.
    let hello = tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = hello else {
        return;
    };
    let (version, kind) = match serde_json::from_str::<ClientMessage>(text.as_str()) {
        Ok(ClientMessage::Hello {
            protocol_version,
            client,
        }) => (protocol_version, client.kind),
        _ => return,
    };
    let reply_err = |m: ServerMessage| serde_json::to_string(&m).expect("serializes");
    if !PROTOCOL_VERSION.is_compatible_with(version) {
        let _ = sink
            .send(Message::Text(reply_err(error_msg("protocol_version", format!("This server speaks AudioNet protocol {PROTOCOL_VERSION}; your client speaks {version}. Update the client."))).into()))
            .await;
        return;
    }
    if (kind == ClientKind::Node) != p.node.is_some() {
        let _ = sink
            .send(Message::Text(
                reply_err(error_msg(
                    "wrong_client_kind",
                    "Devices must sign in with a device token; browsers with a web session.",
                ))
                .into(),
            ))
            .await;
        return;
    }

    let conn_id = format!("c_{}", &new_token("")[..12]);
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(QUEUE_DEPTH);
    s.hub
        .register(&conn_id, p.user_id, p.node.clone(), tx.clone());
    if let Some(n) = &p.node {
        let id = n.id.clone();
        let _ = s.db.call(move |c| db::touch_node(c, &id)).await;
        tracing::info!(conn = %conn_id, node = %n.id, user = %p.username, "device connected");
    } else {
        tracing::info!(conn = %conn_id, user = %p.username, "browser connected");
    }
    let welcome = ServerMessage::Welcome {
        protocol_version: PROTOCOL_VERSION,
        connection_id: conn_id.clone(),
        username: p.username.clone(),
        node_id: p.node.as_ref().and_then(|n| NodeId::new(n.id.clone()).ok()),
        ice_servers: ice_servers(&s, &p),
    };
    let _ = tx.try_send(welcome);

    let mut ping = tokio::time::interval(WS_PING);
    ping.tick().await;
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(msg) => {
                    let text = serde_json::to_string(&msg).expect("serializes");
                    if sink.send(Message::Text(text.into())).await.is_err() { break; }
                }
                None => break, // hub dropped us (slow client or replaced)
            },
            inc = stream.next() => match inc {
                Some(Ok(Message::Text(text))) => {
                    let reply = handle(&s, &p, &conn_id, text.as_str()).await;
                    if let Some(r) = reply { let _ = tx.try_send(r); }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    s.hub.unregister(&conn_id);
    tracing::info!(conn = %conn_id, "disconnected");
}

async fn handle(s: &Shared, p: &Principal, conn_id: &str, text: &str) -> Option<ServerMessage> {
    let msg = match serde_json::from_str::<ClientMessage>(text) {
        Ok(m) => m,
        Err(e) => {
            return Some(error_msg(
                "bad_message",
                format!("Could not understand the message: {e}"),
            ));
        }
    };
    let result = match msg {
        ClientMessage::Hello { .. } => Ok(()),
        ClientMessage::Ping { nonce } => return Some(ServerMessage::Pong { nonce }),
        ClientMessage::Sharing { sharing } => s.hub.set_sharing(conn_id, sharing),
        ClientMessage::Endpoints {
            sources,
            destinations,
        } => {
            if sources.len() > 256 || destinations.len() > 256 {
                return Some(error_msg("too_many", "Too many endpoints."));
            }
            s.hub.set_endpoints(conn_id, sources, destinations)
        }
        ClientMessage::ListNodes => {
            let uid = p.user_id;
            let rows = match s.db.call(move |c| db::nodes_for_user(c, uid)).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("listing nodes: {e}");
                    return Some(error_msg("internal", "Could not list devices."));
                }
            };
            let metas: Vec<NodeMeta> = rows
                .into_iter()
                .map(|r| NodeMeta {
                    platform: platform_from(r.platform.as_deref()),
                    id: r.id,
                    name: r.name,
                })
                .collect();
            return Some(ServerMessage::Nodes {
                nodes: s.hub.summaries(&metas),
            });
        }
        ClientMessage::SessionOffer {
            session_id,
            node_id,
            media,
            sdp,
        } => {
            // A refused offer names its session, so the offerer can end it
            // with the reason.
            let id = session_id.clone();
            return s
                .hub
                .offer(conn_id, session_id, &node_id, media, sdp)
                .err()
                .map(|e| ServerMessage::Error {
                    code: e.code.into(),
                    message: e.message,
                    session_id: Some(id),
                });
        }
        ClientMessage::SessionAnswer { session_id, sdp } => s.hub.answer(conn_id, session_id, sdp),
        // Progress or an end for a session that is already over (refused,
        // or ended by the other side, while this message was on its way):
        // nothing to do, and nothing to report.
        ClientMessage::SessionStatus {
            session_id,
            state,
            detail,
        } => s
            .hub
            .status(conn_id, session_id, state, detail)
            .or_else(already_over),
        ClientMessage::SessionEnd { session_id, reason } => {
            s.hub.end(conn_id, session_id, reason).or_else(already_over)
        }
    };
    result.err().map(|e| error_msg(e.code, e.message))
}
