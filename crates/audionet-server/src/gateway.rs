//! A push gateway (only on a server with the app's APNs key, see
//! `[push_gateway]`): relays notifications from AudioNet servers to Apple.
//!
//! * `POST /push/v1/register` `{apns_token, sandbox}` → `{handle}`: the app
//!   trades its Apple device token for an opaque handle, which it gives its
//!   servers instead. Handles are stored hashed, like every token.
//! * `POST /push/v1/send` `{handle, payload, collapse_id}`: a server sends a
//!   notification; `payload` is encrypted for the app (the gateway and Apple
//!   cannot read it). `410` means the handle is gone: forget it.
//! * `POST /push/v1/unregister` `{handle}`.

use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;

use crate::api::{ApiError, ClientAddress, Shared};
use crate::apns::Outcome;
use crate::auth::{hash_token, new_token};
use crate::db;

/// Phones registering per client address per hour.
const REGISTRATIONS_PER_ADDRESS: u32 = 30;
/// Notifications per handle per hour, and for the whole gateway.
const SENDS_PER_HANDLE: u32 = 120;
const SENDS_PER_HOUR: u32 = 20_000;

fn gateway_off() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "no_gateway",
        "This server is not a push gateway.",
    )
}

#[derive(Deserialize)]
pub(crate) struct Register {
    apns_token: String,
    #[serde(default)]
    sandbox: bool,
}

pub(crate) async fn register(
    State(s): State<Shared>,
    ClientAddress(address): ClientAddress,
    Json(r): Json<Register>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if s.apns.is_none() {
        return Err(gateway_off());
    }
    let token = r.apns_token.trim().to_ascii_lowercase();
    if !(32..=200).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_token",
            "Not an Apple device token.",
        ));
    }
    if s.push_register_limit
        .check(
            &address,
            REGISTRATIONS_PER_ADDRESS,
            u32::MAX,
            Instant::now(),
        )
        .is_err()
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "limit",
            "Too many registrations from your network. Try again later.",
        ));
    }
    s.push_register_limit.record(&address, Instant::now());
    let handle = new_token("anp_");
    let hash = hash_token(&handle);
    let sandbox = r.sandbox;
    s.db.call(move |c| db::set_push_handle(c, &hash, &token, sandbox))
        .await?;
    Ok(Json(json!({ "handle": handle })))
}

#[derive(Deserialize)]
pub(crate) struct Send {
    handle: String,
    payload: String,
    collapse_id: String,
}

pub(crate) async fn send(
    State(s): State<Shared>,
    Json(r): Json<Send>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Some(apns) = &s.apns else {
        return Err(gateway_off());
    };
    if r.payload.len() > 3000 || r.collapse_id.len() > 64 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "too_big",
            "The notification is too big.",
        ));
    }
    let hash = hash_token(&r.handle);
    let lookup = hash.clone();
    let Some((device_token, sandbox)) = s.db.call(move |c| db::push_handle(c, &lookup)).await?
    else {
        return Err(ApiError::new(
            StatusCode::GONE,
            "unregistered",
            "No such push handle.",
        ));
    };
    let key = format!("{:x?}", &hash.0[..8]);
    if s.push_send_limit
        .check(&key, SENDS_PER_HANDLE, SENDS_PER_HOUR, Instant::now())
        .is_err()
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "limit",
            "Too many notifications for this device. Try again later.",
        ));
    }
    s.push_send_limit.record(&key, Instant::now());
    // What Apple and a phone without the app's extension see: a neutral
    // placeholder; the extension replaces it with the decrypted text.
    let aps = json!({
        "aps": {
            "alert": { "title": "AudioNet", "body": "A device changed." },
            "mutable-content": 1,
            "sound": "default",
        },
        "e": r.payload,
    });
    match apns.send(&device_token, sandbox, &r.collapse_id, aps).await {
        Outcome::Sent => Ok(Json(json!({ "sent": true }))),
        Outcome::Unregistered => {
            s.db.call(move |c| db::delete_push_handle(c, &hash)).await?;
            Err(ApiError::new(
                StatusCode::GONE,
                "unregistered",
                "The device no longer receives notifications.",
            ))
        }
        Outcome::Failed(e) => {
            tracing::warn!("push not delivered: {e}");
            Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "apns_failed",
                "Apple's push service did not take the notification.",
            ))
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct Unregister {
    handle: String,
}

pub(crate) async fn unregister(
    State(s): State<Shared>,
    Json(r): Json<Unregister>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if s.apns.is_none() {
        return Err(gateway_off());
    }
    let hash = hash_token(&r.handle);
    s.db.call(move |c| db::delete_push_handle(c, &hash)).await?;
    Ok(Json(json!({ "unregistered": true })))
}
