//! Adding a device to an account on an AudioNet server: the device signs in
//! with the account name and password and receives its own device token.

use audionet_protocol::{PROTOCOL_VERSION, Platform, ProtocolVersion};
use serde::Deserialize;
use serde_json::json;

use crate::config::{NodeConfig, normalize_server_url};

#[derive(Deserialize)]
struct Info {
    name: String,
    protocol_version: ProtocolVersion,
}

#[derive(Deserialize)]
struct SignedIn {
    node_id: String,
    token: String,
    username: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into()
}

/// Checks that `server_url` is an AudioNet server this client can talk to.
pub fn check_server(server_url: &str) -> Result<String, String> {
    let url = normalize_server_url(server_url)?;
    let mut resp = agent()
        .get(format!("{url}/api/v1/info"))
        .call()
        .map_err(|e| format!("could not reach {url}: {e}"))?;
    if resp.status() != 200 {
        return Err(format!(
            "{url} did not answer like an AudioNet server (HTTP {}).",
            resp.status()
        ));
    }
    let info: Info = resp
        .body_mut()
        .read_json()
        .map_err(|_| format!("{url} did not answer like an AudioNet server."))?;
    if info.name != "AudioNet" {
        return Err(format!("{url} is not an AudioNet server."));
    }
    if !PROTOCOL_VERSION.is_compatible_with(info.protocol_version) {
        return Err(format!(
            "{url} speaks AudioNet protocol {}, but this program speaks {PROTOCOL_VERSION}. Update one of them.",
            info.protocol_version
        ));
    }
    Ok(url)
}

/// Signs in with the account password and registers this device (native
/// apps). The password is sent once over HTTPS and never stored; the
/// device keeps only its own token.
pub fn sign_in(
    server_url: &str,
    username: &str,
    password: &str,
    name: &str,
    platform: Platform,
) -> Result<NodeConfig, String> {
    let url = check_server(server_url)?;
    let body = json!({
        "username": username, "password": password, "name": name, "platform": platform
    });
    register(&url, "sign-in", body, name)
}

/// Posts to `/api/v1/nodes/{endpoint}` and turns the device credential
/// into a configuration.
fn register(
    url: &str,
    endpoint: &str,
    body: serde_json::Value,
    name: &str,
) -> Result<NodeConfig, String> {
    let mut resp = agent()
        .post(format!("{url}/api/v1/nodes/{endpoint}"))
        .send_json(body)
        .map_err(|e| format!("could not reach {url}: {e}"))?;
    if resp.status() != 200 {
        let msg = resp
            .body_mut()
            .read_json::<ErrorBody>()
            .map(|b| b.error.message)
            .unwrap_or_else(|_| format!("the server answered HTTP {}", resp.status()));
        return Err(msg);
    }
    let p: SignedIn = resp
        .body_mut()
        .read_json()
        .map_err(|e| format!("unexpected answer from the server: {e}"))?;
    Ok(NodeConfig {
        server_url: url.to_owned(),
        node_id: p.node_id,
        token: p.token,
        name: name.to_owned(),
        username: p.username,
    })
}

/// Removes this device from its account on the server (signing out): its
/// record and token are deleted, so nothing is left behind. A device the
/// server no longer knows counts as removed.
pub fn remove_device(config: &NodeConfig) -> Result<(), String> {
    let url = normalize_server_url(&config.server_url)?;
    let resp = agent()
        .delete(format!("{url}/api/v1/nodes/{}", config.node_id))
        .header("Authorization", &format!("Bearer {}", config.token))
        .call()
        .map_err(|e| format!("could not reach {url}: {e}"))?;
    match resp.status().as_u16() {
        200 | 204 | 401 | 404 => Ok(()),
        other => {
            let mut resp = resp;
            Err(resp
                .body_mut()
                .read_json::<ErrorBody>()
                .map(|b| b.error.message)
                .unwrap_or_else(|_| format!("the server answered HTTP {other}")))
        }
    }
}
