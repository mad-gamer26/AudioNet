//! A minimal STUN Binding client (RFC 8489 §5–§14.2), used once per
//! session on the media socket to learn its server-reflexive (public)
//! address for ICE.
//!
//! This is deliberately the smallest piece of NAT traversal: one Binding
//! request, parse XOR-MAPPED-ADDRESS. Connectivity checks, TURN relaying
//! and everything else stay with the WebRTC stack and the TURN server.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

const MAGIC: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Builds a Binding request with the given transaction id.
pub fn binding_request(txid: &[u8; 12]) -> [u8; 20] {
    let mut m = [0u8; 20];
    m[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    // length 0: no attributes
    m[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    m[8..20].copy_from_slice(txid);
    m
}

/// Parses a Binding success response for `txid`; returns the mapped address.
pub fn parse_binding_response(msg: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if msg.len() < 20
        || u16::from_be_bytes([msg[0], msg[1]]) != BINDING_SUCCESS
        || u32::from_be_bytes([msg[4], msg[5], msg[6], msg[7]]) != MAGIC
        || &msg[8..20] != txid
    {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([msg[2], msg[3]]));
    let body = msg.get(20..20 + len)?;
    let mut i = 0;
    while i + 4 <= body.len() {
        let t = u16::from_be_bytes([body[i], body[i + 1]]);
        let l = usize::from(u16::from_be_bytes([body[i + 2], body[i + 3]]));
        let v = body.get(i + 4..i + 4 + l)?;
        if t == XOR_MAPPED_ADDRESS && l >= 8 {
            let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC >> 16) as u16;
            let ip = match v[1] {
                0x01 => {
                    let x = u32::from_be_bytes([v[4], v[5], v[6], v[7]]) ^ MAGIC;
                    IpAddr::V4(Ipv4Addr::from(x))
                }
                0x02 if l >= 20 => {
                    let mut key = [0u8; 16];
                    key[..4].copy_from_slice(&MAGIC.to_be_bytes());
                    key[4..].copy_from_slice(txid);
                    let mut a = [0u8; 16];
                    for k in 0..16 {
                        a[k] = v[4 + k] ^ key[k];
                    }
                    IpAddr::V6(Ipv6Addr::from(a))
                }
                _ => return None,
            };
            return Some(SocketAddr::new(ip, port));
        }
        i += 4 + l.div_ceil(4) * 4;
    }
    None
}

/// Resolves a `stun:host:port` URL (default port 3478) to an IPv4 address.
pub fn resolve_stun_url(url: &str) -> Option<SocketAddr> {
    let rest = url
        .strip_prefix("stun:")
        .or_else(|| url.strip_prefix("turn:"))?;
    let rest = rest.split('?').next()?;
    let target = if rest
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        rest.to_owned()
    } else {
        format!("{rest}:3478")
    };
    target.to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)
}

/// Sends a Binding request from `socket` to `server` and waits briefly for
/// the answer. Control path (before the media loop starts).
pub fn query(socket: &UdpSocket, server: SocketAddr) -> Option<SocketAddr> {
    let mut txid = [0u8; 12];
    rand::fill(&mut txid);
    let req = binding_request(&txid);
    let old = socket.read_timeout().ok().flatten();
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let mut buf = [0u8; 512];
    let mut result = None;
    'tries: for _ in 0..3 {
        if socket.send_to(&req, server).is_err() {
            break;
        }
        while let Ok((n, from)) = socket.recv_from(&mut buf) {
            if from == server {
                if let Some(a) = parse_binding_response(&buf[..n], &txid) {
                    result = Some(a);
                    break 'tries;
                }
            }
        }
    }
    let _ = socket.set_read_timeout(old);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc5769_ipv4_response() {
        // RFC 5769 §2.2 sample response (XOR-MAPPED-ADDRESS 192.0.2.1:32853).
        let txid: [u8; 12] = [
            0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae,
        ];
        let mut msg = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
        msg.extend_from_slice(&txid);
        msg.extend_from_slice(&[
            0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43,
        ]);
        assert_eq!(
            parse_binding_response(&msg, &txid),
            Some("192.0.2.1:32853".parse().unwrap())
        );
        let mut other = txid;
        other[0] ^= 1;
        assert_eq!(
            parse_binding_response(&msg, &other),
            None,
            "wrong transaction"
        );
    }

    #[test]
    fn builds_requests_and_resolves_urls() {
        let r = binding_request(&[7; 12]);
        assert_eq!(&r[..4], &[0, 1, 0, 0]);
        assert_eq!(&r[4..8], &MAGIC.to_be_bytes());
        assert_eq!(
            resolve_stun_url("stun:127.0.0.1:3479"),
            Some("127.0.0.1:3479".parse().unwrap())
        );
        assert_eq!(
            resolve_stun_url("stun:127.0.0.1"),
            Some("127.0.0.1:3478".parse().unwrap())
        );
        assert_eq!(resolve_stun_url("http://x"), None);
    }
}
