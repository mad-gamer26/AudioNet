//! TURN relay for native sessions (RFC 8656): one UDP allocation on the
//! TURN server the coordination server names, through the session's own
//! UDP socket. The TURN protocol itself is `turn-client-proto` (sans-I/O);
//! this module moves its packets and hands relayed data to ICE.
//!
//! Phones and computers behind strict NATs cannot always reach each other
//! directly. With a relayed candidate, ICE can still connect through the
//! TURN server, which forwards the already DTLS-SRTP encrypted packets
//! without being able to read them.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use audionet_protocol::signal::IceServer;
use turn_client_proto::api::{TurnClientApi, TurnConfig, TurnEvent, TurnRecvRet};
use turn_client_proto::stun::Instant as TurnInstant;
use turn_client_proto::stun::agent::Transmit;
use turn_client_proto::types::{TransportType, TurnCredentials};
use turn_client_proto::udp::TurnClientUdp;

/// A TURN server and the short-lived credentials to use it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnServer {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

/// The first UDP TURN server in the ICE configuration, if any. `turns:`
/// (TLS) and `?transport=tcp` entries are skipped.
pub fn find_turn(ice_servers: &[IceServer]) -> Option<TurnServer> {
    ice_servers.iter().find_map(|s| {
        let (username, password) = (s.username.clone()?, s.credential.clone()?);
        s.urls.iter().find_map(|url| {
            let (host, port) = parse_turn_url(url)?;
            Some(TurnServer {
                host,
                port,
                username: username.clone(),
                password: password.clone(),
            })
        })
    })
}

/// `turn:host[:port][?transport=udp]` → (host, port). Only plain UDP.
fn parse_turn_url(url: &str) -> Option<(String, u16)> {
    let rest = url.strip_prefix("turn:")?;
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    if !query.is_empty() && query != "transport=udp" {
        return None;
    }
    match addr.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !host.ends_with(']') => {
            Some((host.to_owned(), port.parse().ok()?))
        }
        _ if !addr.is_empty() => Some((addr.to_owned(), 3478)),
        _ => None,
    }
}

/// An allocation on a TURN server.
pub struct Relay {
    client: TurnClientUdp,
    server: SocketAddr,
    local: SocketAddr,
    relayed: SocketAddr,
    base: Instant,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("server", &self.server)
            .field("relayed", &self.relayed)
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// Resolves the server and allocates a relayed address, waiting up to
    /// `timeout`. `local` is this socket's address as candidates use it.
    pub fn allocate(
        socket: &UdpSocket,
        local: SocketAddr,
        server: &TurnServer,
        timeout: Duration,
    ) -> Result<Relay, String> {
        let server_addr = (server.host.as_str(), server.port)
            .to_socket_addrs()
            .map_err(|e| format!("could not find the relay server {}: {e}", server.host))?
            .find(SocketAddr::is_ipv4)
            .ok_or_else(|| format!("the relay server {} has no IPv4 address", server.host))?;
        let config = TurnConfig::new(TurnCredentials::new(&server.username, &server.password));
        let mut relay = Relay {
            client: TurnClientUdp::allocate(local, server_addr, config),
            server: server_addr,
            local,
            relayed: local,
            base: Instant::now(),
        };
        let deadline = Instant::now() + timeout;
        let old_timeout = socket.read_timeout().ok().flatten();
        let mut buf = vec![0u8; 2048];
        let mut challenge: Option<[u8; 12]> = None;
        let result = loop {
            let now = Instant::now();
            if now >= deadline {
                break Err("the relay server did not answer".to_owned());
            }
            relay.flush(socket, now);
            match relay.client.poll_event() {
                Some(TurnEvent::AllocationCreated(TransportType::Udp, addr)) => {
                    relay.relayed = addr;
                    break Ok(());
                }
                Some(TurnEvent::AllocationCreateFailed(_)) => {
                    break Err("the relay server refused the allocation".to_owned());
                }
                _ => {}
            }
            let _ = socket.set_read_timeout(Some(
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(50))
                    .max(Duration::from_millis(1)),
            ));
            if let Ok((n, from)) = socket.recv_from(&mut buf)
                && from == relay.server
            {
                // turn-client-proto ignores an error the server cannot
                // sign, which is every refusal of our credentials (it
                // cannot sign with credentials it rejects), and would
                // keep retrying until `timeout`. Stop at once instead.
                if let Some(why) = allocate_refusal(&buf[..n], &mut challenge) {
                    break Err(why);
                }
                let _ = relay.receive(socket, &buf[..n], Instant::now());
            }
        };
        let _ = socket.set_read_timeout(old_timeout);
        result.map(|()| relay)
    }

    fn now(&self, at: Instant) -> TurnInstant {
        TurnInstant::from_nanos(at.saturating_duration_since(self.base).as_nanos() as i64)
    }

    /// The relayed address (the candidate peers send to).
    pub fn relayed(&self) -> SocketAddr {
        self.relayed
    }

    pub fn server(&self) -> SocketAddr {
        self.server
    }

    /// Sends `data` to `peer` through the relay, first installing a
    /// permission for the peer if needed (ICE retries its checks, so early
    /// packets dropped before the permission exists are harmless).
    pub fn send(&mut self, socket: &UdpSocket, peer: SocketAddr, data: &[u8], at: Instant) {
        let now = self.now(at);
        if !self.client.have_permission(TransportType::Udp, peer.ip()) {
            let _ = self
                .client
                .create_permission(TransportType::Udp, peer.ip(), now);
        }
        if let Ok(Some(transmit)) = self.client.send_to(TransportType::Udp, peer, data, now) {
            let t = transmit.build();
            let _ = socket.send_to(&t.data, t.to);
        }
        self.flush(socket, at);
    }

    /// Handles a datagram from the TURN server. Returns data relayed from
    /// peers, as (peer address, bytes), for ICE.
    pub fn receive(
        &mut self,
        socket: &UdpSocket,
        data: &[u8],
        at: Instant,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        let now = self.now(at);
        let mut out = Vec::new();
        let transmit = Transmit::new(data, TransportType::Udp, self.server, self.local);
        if let TurnRecvRet::PeerData(d) = self.client.recv(transmit, now) {
            out.push((d.peer, d.data().to_vec()));
        }
        while let Some(d) = self.client.poll_recv(now) {
            out.push((d.peer, d.data().to_vec()));
        }
        self.flush(socket, at);
        out
    }

    /// Runs timers (allocation and permission refreshes). Call often.
    pub fn tick(&mut self, socket: &UdpSocket, at: Instant) {
        let _ = self.client.poll(self.now(at));
        self.flush(socket, at);
    }

    fn flush(&mut self, socket: &UdpSocket, at: Instant) {
        let now = self.now(at);
        let _ = self.client.poll(now);
        while let Some(t) = self.client.poll_transmit(now) {
            let _ = socket.send_to(t.data.as_ref(), t.to);
        }
    }

    /// Whether a datagram came from the TURN server.
    pub fn is_from_server(&self, from: SocketAddr) -> bool {
        from == self.server
    }
}

/// STUN Allocate error response (RFC 8656 / RFC 8489 message type).
const ALLOCATE_ERROR: u16 = 0x0113;
const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
const ATTR_ERROR_CODE: u16 = 0x0009;

/// Whether a datagram from the TURN server refuses our credentials.
///
/// An Allocate starts unauthenticated and the server answers 401 with its
/// realm and nonce: that first 401 (whose transaction id is remembered in
/// `challenge`) is normal. A 401 to any later transaction answers a request
/// that carried our credentials: the server refused them (most often
/// because they expired). Anything else returns `None`.
fn allocate_refusal(msg: &[u8], challenge: &mut Option<[u8; 12]>) -> Option<String> {
    if msg.len() < 20
        || u16::from_be_bytes([msg[0], msg[1]]) != ALLOCATE_ERROR
        || msg[4..8] != MAGIC_COOKIE
    {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([msg[2], msg[3]]));
    let attrs = msg.get(20..20 + len)?;
    let mut txn = [0u8; 12];
    txn.copy_from_slice(&msg[8..20]);
    let mut at = 0;
    let mut code = None;
    while at + 4 <= attrs.len() {
        let kind = u16::from_be_bytes([attrs[at], attrs[at + 1]]);
        let alen = usize::from(u16::from_be_bytes([attrs[at + 2], attrs[at + 3]]));
        let value = attrs.get(at + 4..at + 4 + alen)?;
        if kind == ATTR_ERROR_CODE && value.len() >= 4 {
            code = Some(u16::from(value[2] & 0x07) * 100 + u16::from(value[3]));
        }
        at += 4 + alen.div_ceil(4) * 4;
    }
    if code != Some(401) {
        return None;
    }
    match challenge {
        None => {
            *challenge = Some(txn);
            None
        }
        Some(first) if *first == txn => None, // the challenge, repeated
        Some(_) => Some(
            "the relay server refused this device's credentials (401 Unauthorized; they may have expired)".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_udp_turn_server() {
        let ice = vec![
            IceServer {
                urls: vec!["stun:audionet.example.com:3478".into()],
                username: None,
                credential: None,
            },
            IceServer {
                urls: vec![
                    "turns:audionet.example.com:5349?transport=tcp".into(),
                    "turn:audionet.example.com:3478?transport=tcp".into(),
                    "turn:audionet.example.com:3478?transport=udp".into(),
                ],
                username: Some("1700000000:alice".into()),
                credential: Some("secret".into()),
            },
        ];
        let t = find_turn(&ice).unwrap();
        assert_eq!((t.host.as_str(), t.port), ("audionet.example.com", 3478));
        assert_eq!(t.username, "1700000000:alice");
    }

    #[test]
    fn parses_turn_urls() {
        assert_eq!(
            parse_turn_url("turn:relay.example.com"),
            Some(("relay.example.com".into(), 3478))
        );
        assert_eq!(
            parse_turn_url("turn:relay.example.com:3479"),
            Some(("relay.example.com".into(), 3479))
        );
        assert_eq!(parse_turn_url("turn:relay.example.com?transport=tcp"), None);
        assert_eq!(parse_turn_url("stun:relay.example.com"), None);
        assert_eq!(parse_turn_url("turn:"), None);
    }

    #[test]
    fn no_credentials_no_relay() {
        let ice = vec![IceServer {
            urls: vec!["turn:relay.example.com".into()],
            username: None,
            credential: None,
        }];
        assert_eq!(find_turn(&ice), None);
    }

    /// A STUN Allocate error response with ERROR-CODE `code`, REALM and
    /// NONCE, as coturn sends.
    fn allocate_error(txn: [u8; 12], code: u16) -> Vec<u8> {
        fn attr(out: &mut Vec<u8>, kind: u16, value: &[u8]) {
            out.extend_from_slice(&kind.to_be_bytes());
            out.extend_from_slice(&(value.len() as u16).to_be_bytes());
            out.extend_from_slice(value);
            out.resize(out.len().div_ceil(4) * 4, 0);
        }
        let mut attrs = Vec::new();
        let mut ec = vec![0, 0, (code / 100) as u8, (code % 100) as u8];
        ec.extend_from_slice(b"Unauthorized");
        attr(&mut attrs, 0x0009, &ec);
        attr(&mut attrs, 0x0014, b"test.example");
        attr(&mut attrs, 0x0015, b"0123456789abcdef");
        let mut m = Vec::new();
        m.extend_from_slice(&ALLOCATE_ERROR.to_be_bytes());
        m.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
        m.extend_from_slice(&MAGIC_COOKIE);
        m.extend_from_slice(&txn);
        m.extend_from_slice(&attrs);
        m
    }

    #[test]
    fn a_second_401_is_a_refusal_not_a_challenge() {
        let mut challenge = None;
        // The first 401 challenges (realm and nonce); repeated, still fine.
        assert_eq!(
            allocate_refusal(&allocate_error([1; 12], 401), &mut challenge),
            None
        );
        assert_eq!(
            allocate_refusal(&allocate_error([1; 12], 401), &mut challenge),
            None
        );
        // Another error (stale nonce) is left to the TURN client.
        assert_eq!(
            allocate_refusal(&allocate_error([2; 12], 438), &mut challenge),
            None
        );
        // A 401 to the request that carried the credentials: refused.
        let why = allocate_refusal(&allocate_error([2; 12], 401), &mut challenge).unwrap();
        assert!(why.contains("refused"), "{why}");
        // Not an Allocate error, or not STUN at all.
        let mut fresh = None;
        let mut success = allocate_error([3; 12], 401);
        success[1] = 0x03; // Allocate success response
        assert_eq!(allocate_refusal(&success, &mut fresh), None);
        assert_eq!(allocate_refusal(b"not stun", &mut fresh), None);
        assert_eq!(
            allocate_refusal(&allocate_error([4; 12], 401)[..30], &mut fresh),
            None
        );
        assert_eq!(fresh, None);
    }

    /// A TURN server that refuses the credentials (as coturn does once they
    /// have expired): allocation fails at once instead of after `timeout`.
    #[test]
    fn refused_credentials_fail_fast() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let refuser = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            let mut answered = 0;
            while let Ok((n, from)) = server.recv_from(&mut buf) {
                if n >= 20 && buf[0..2] == [0x00, 0x03] {
                    let mut txn = [0u8; 12];
                    txn.copy_from_slice(&buf[8..20]);
                    let _ = server.send_to(&allocate_error(txn, 401), from);
                    answered += 1;
                    if answered == 4 {
                        break;
                    }
                }
            }
            answered
        });
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let local = socket.local_addr().unwrap();
        let turn = TurnServer {
            host: "127.0.0.1".into(),
            port: server_addr.port(),
            username: "1000:expired".into(),
            password: "c2VjcmV0".into(),
        };
        let started = Instant::now();
        let err = Relay::allocate(&socket, local, &turn, Duration::from_secs(3)).unwrap_err();
        let took = started.elapsed();
        assert!(err.contains("refused"), "{err}");
        assert!(
            took < Duration::from_millis(500),
            "took {took:?}, not failing fast"
        );
        drop(socket);
        assert!(
            refuser.join().unwrap() >= 2,
            "challenge and the refused retry"
        );
    }
}
