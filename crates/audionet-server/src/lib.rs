//! The AudioNet coordination server, as a library so it can be tested
//! in-process. See `docs/self-hosting.md` for deployment.

#![forbid(unsafe_code)]

pub mod admin;
pub mod api;
pub mod apns;
pub mod auth;
pub mod client_version;
pub mod config;
pub mod db;
pub mod gateway;
pub mod http_client;
pub mod hub;
pub mod logbuf;
pub mod mail;
pub mod push;

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;

/// Conservative security headers for every response. The web client uses
/// only same-origin scripts, styles and connections.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
             connect-src 'self'; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'self'; \
             form-action 'self'",
        ),
    );
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("microphone=(self), camera=(), geolocation=()"),
    );
    // Browsers check with the server before reusing a page file (a cheap
    // 304 when unchanged), so an updated web client is used at once.
    // Without this they guess how long to keep it, often for hours.
    if !h.contains_key(axum::http::header::CACHE_CONTROL) {
        h.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        );
    }
    resp
}
