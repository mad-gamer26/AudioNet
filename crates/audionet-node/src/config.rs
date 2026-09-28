//! The node's saved configuration: which server it belongs to and its
//! device credential.
//!
//! Stored as TOML in the user's configuration directory
//! (`%APPDATA%\AudioNet\node.toml` on Windows,
//! `$XDG_CONFIG_HOME/audionet/node.toml` or `~/.config/audionet/node.toml`
//! elsewhere). The token is a secret: on Unix the file is created mode 0600;
//! on Windows the per-user profile directory is private by default.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeConfig {
    /// Base URL of the AudioNet server, e.g. `https://audionet.example.com`.
    pub server_url: String,
    pub node_id: String,
    /// Device credential issued at sign-in (`ann_…`). Secret.
    pub token: String,
    /// Display name chosen at sign-in.
    pub name: String,
    /// Account the device belongs to.
    pub username: String,
}

impl std::fmt::Debug for NodeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeConfig")
            .field("server_url", &self.server_url)
            .field("node_id", &self.node_id)
            .field("token", &"<redacted>")
            .field("name", &self.name)
            .field("username", &self.username)
            .finish()
    }
}

pub fn default_path() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("AudioNet").join("node.toml");
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("audionet").join("node.toml");
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".config").join("audionet").join("node.toml")
}

impl NodeConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "could not read {}: {e}. Sign this device in first with `audionet node sign-in`.",
                path.display()
            )
        })?;
        toml::from_str(&text)
            .map_err(|e| format!("{} is not a valid node configuration: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        write_private(path, text.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    }

    /// The WebSocket URL for signaling.
    pub fn ws_url(&self) -> String {
        ws_url(&self.server_url)
    }
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

/// Normalizes and checks a server URL. HTTPS is required, except plain
/// HTTP to the local machine (for development and tests).
pub fn normalize_server_url(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let (scheme, rest) = url.split_once("://").ok_or_else(|| {
        format!("{url} is not a URL; use a form like https://audionet.example.com")
    })?;
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    match scheme {
        "https" => Ok(url.to_owned()),
        "http" if matches!(host, "localhost" | "127.0.0.1" | "[::1]") => Ok(url.to_owned()),
        "http" => Err(
            "AudioNet servers must use https:// (plain http is only allowed for localhost testing)"
                .into(),
        ),
        _ => Err(format!("unsupported URL scheme {scheme}; use https://")),
    }
}

pub fn ws_url(server_url: &str) -> String {
    let base = server_url.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_owned()
    };
    format!("{ws}/api/v1/ws")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            normalize_server_url("https://audio.example.com/").unwrap(),
            "https://audio.example.com"
        );
        assert!(normalize_server_url("http://audio.example.com").is_err());
        assert!(normalize_server_url("http://127.0.0.1:8740").is_ok());
        assert!(normalize_server_url("audio.example.com").is_err());
        assert_eq!(
            ws_url("https://a.example.com/base"),
            "wss://a.example.com/base/api/v1/ws"
        );
        assert_eq!(
            ws_url("http://localhost:8740"),
            "ws://localhost:8740/api/v1/ws"
        );
    }

    #[test]
    fn debug_redacts_token() {
        let c = NodeConfig {
            server_url: "https://x".into(),
            node_id: "n".into(),
            token: "ann_secret".into(),
            name: "PC".into(),
            username: "u".into(),
        };
        assert!(!format!("{c:?}").contains("ann_secret"));
        let dir = std::env::temp_dir().join(format!("audionet-test-{}", std::process::id()));
        let path = dir.join("node.toml");
        c.save(&path).unwrap();
        assert_eq!(NodeConfig::load(&path).unwrap(), c);
        let _ = std::fs::remove_dir_all(dir);
    }
}
