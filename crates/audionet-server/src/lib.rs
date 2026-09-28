//! The AudioNet coordination server, as a library so it can be tested
//! in-process. See `docs/self-hosting.md` for deployment.

#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod hub;
pub mod mail;

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
    resp
}
