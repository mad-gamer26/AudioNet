//! The line protocol between the NVDA add-on and this program.
//!
//! Standard input: one JSON request per line,
//! `{"id": 7, "cmd": "listen", ...}`. Standard output: one JSON line per
//! reply (`{"id": 7, "ok": true, "result": {...}, "error": null}`) or event
//! (`{"event": "session", "account": "...", ...}`), UTF-8. When standard
//! input closes (NVDA exited, or crashed), every stream stops and the
//! program exits.
//!
//! Accounts are named by the add-on (`account`, any string) and connect
//! with a web session token (`sign_in` gets one from the password, which is
//! used once and never kept).

use serde::Deserialize;

/// A request from the add-on.
#[derive(Debug, Deserialize, PartialEq)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub cmd: Cmd,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Cmd {
    /// The program's version.
    Hello,
    /// Signs in with the password: `{server, username, token}` (a web
    /// session; no device is added to the account).
    SignIn {
        server: String,
        username: String,
        password: String,
    },
    /// Ends a web session on the server.
    SignOut {
        server: String,
        token: String,
    },
    /// Connects an account (replacing an earlier connection of the same name).
    Connect {
        account: String,
        server: String,
        username: String,
        token: String,
    },
    Disconnect {
        account: String,
    },
    /// Asks again for an account's devices (a `devices` event follows).
    ListDevices {
        account: String,
    },
    /// This computer's microphones, sounds and outputs:
    /// `{sources: [SourceInfo], destinations: [DestinationInfo]}`.
    LocalAudio,
    /// Listens to a device's sound on this computer's output:
    /// `{session_id}`.
    Listen {
        account: String,
        node_id: String,
        source_id: String,
        destination_id: String,
    },
    /// Sends this computer's source to a device's output: `{session_id}`.
    Send {
        account: String,
        node_id: String,
        destination_id: String,
        source_id: String,
    },
    Stop {
        session_id: String,
    },
    /// A stream's volume on this computer (0.0 to 1.0, the slider position)
    /// and mute.
    SetVolume {
        session_id: String,
        volume: f32,
        muted: bool,
    },
    /// Stops everything and exits.
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests() {
        let r: Request = serde_json::from_str(
            r#"{"id":3,"cmd":"listen","account":"a1","node_id":"node_x","source_id":"loopback:1","destination_id":"output:2"}"#,
        )
        .unwrap();
        assert_eq!(
            r,
            Request {
                id: 3,
                cmd: Cmd::Listen {
                    account: "a1".into(),
                    node_id: "node_x".into(),
                    source_id: "loopback:1".into(),
                    destination_id: "output:2".into(),
                }
            }
        );
        let r: Request = serde_json::from_str(r#"{"id":1,"cmd":"hello"}"#).unwrap();
        assert_eq!(r.cmd, Cmd::Hello);
        let r: Request = serde_json::from_str(
            r#"{"id":2,"cmd":"set_volume","session_id":"s","volume":0.5,"muted":false}"#,
        )
        .unwrap();
        assert!(matches!(r.cmd, Cmd::SetVolume { volume, .. } if volume == 0.5));
        assert!(serde_json::from_str::<Request>(r#"{"id":1,"cmd":"format_disk"}"#).is_err());
        assert!(
            serde_json::from_str::<Request>(r#"{"cmd":"hello"}"#).is_err(),
            "needs an id"
        );
    }
}
