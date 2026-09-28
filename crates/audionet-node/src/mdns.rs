//! Finds the local addresses that browsers hide behind `<uuid>.local`
//! names in their WebRTC candidates (RFC 8828 and draft-ietf-mmusic-mdns-
//! ice-candidates), with one-shot multicast DNS queries (RFC 6762 §5.1).
//!
//! Why: a browser that has not been given microphone access (someone only
//! listening) never reveals its local address. Without it this device
//! cannot send to the browser on the local network, so a firewall that
//! drops unsolicited packets (Windows by default) also drops the
//! browser's own checks, and the connection falls back to the TURN relay
//! even when both are on the same network. Sending to the resolved address
//! opens the direct path.
//!
//! Runs on a session's control thread, never on an audio thread: it opens
//! a socket, sends a few small queries and waits at most `timeout`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

const MDNS_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
/// A browser offers a handful of candidates; anything more is not a browser.
const MAX_NAMES: usize = 8;

/// A remote host candidate with a `.local` name, from an SDP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HiddenCandidate {
    pub name: String,
    pub port: u16,
}

/// The UDP host candidates in `sdp` whose address is a `.local` name.
pub fn hidden_candidates(sdp: &str) -> Vec<HiddenCandidate> {
    let mut out: Vec<HiddenCandidate> = Vec::new();
    for line in sdp.lines() {
        let Some(rest) = line.trim().strip_prefix("a=candidate:") else {
            continue;
        };
        // foundation component transport priority address port "typ" type
        let f: Vec<&str> = rest.split_whitespace().collect();
        if f.len() < 8 || !f[2].eq_ignore_ascii_case("udp") || f[6] != "typ" || f[7] != "host" {
            continue;
        }
        let name = f[4].trim_end_matches('.').to_ascii_lowercase();
        let valid_name = name.len() <= 100
            && name.ends_with(".local")
            && name.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        let Ok(port) = f[5].parse::<u16>() else {
            continue;
        };
        let c = HiddenCandidate { name, port };
        if valid_name && port != 0 && !out.contains(&c) && out.len() < MAX_NAMES {
            out.push(c);
        }
    }
    out
}

/// A query for the IPv4 address of `name`. `unicast` sets the "QU" bit,
/// asking for an answer sent straight back to the asking port (for queries
/// not sent from port 5353; Apple's mDNSResponder answers these, Chrome's
/// responder does not).
pub fn query_packet(name: &str, unicast: bool) -> Vec<u8> {
    let mut p = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]; // id 0, flags 0, 1 question
    for label in name.split('.') {
        p.push(label.len() as u8);
        p.extend_from_slice(label.as_bytes());
    }
    p.push(0);
    p.extend_from_slice(&[0, 1, if unicast { 0x80 } else { 0 }, 1]); // type A, class IN
    p
}

/// Reads a (possibly compressed) name at `pos`; returns it and the
/// position after it in the record.
fn read_name(p: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    for _ in 0..64 {
        let len = *p.get(pos)? as usize;
        match len {
            0 => {
                return Some((labels.join("."), end.unwrap_or(pos + 1)));
            }
            l if l & 0xC0 == 0xC0 => {
                let target = ((l & 0x3F) << 8) | *p.get(pos + 1)? as usize;
                end.get_or_insert(pos + 2);
                pos = target;
            }
            l if l < 64 => {
                let label = p.get(pos + 1..pos + 1 + l)?;
                labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
                pos += 1 + l;
            }
            _ => return None,
        }
    }
    None // a pointer loop
}

/// The IPv4 addresses a DNS response gives for `name` (answers and
/// additional records).
pub fn parse_answers(p: &[u8], name: &str) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let count = |i: usize| {
        p.get(i..i + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
    };
    let (Some(qd), Some(an), Some(ns), Some(ar)) = (count(4), count(6), count(8), count(10)) else {
        return out;
    };
    // Responses only.
    if p.get(2).is_none_or(|f| f & 0x80 == 0) {
        return out;
    }
    let mut pos = 12;
    for _ in 0..qd {
        let Some((_, next)) = read_name(p, pos) else {
            return out;
        };
        pos = next + 4;
    }
    for _ in 0..an + ns + ar {
        let Some((rname, next)) = read_name(p, pos) else {
            return out;
        };
        let Some(h) = p.get(next..next + 10) else {
            return out;
        };
        let rtype = u16::from_be_bytes([h[0], h[1]]);
        let len = u16::from_be_bytes([h[8], h[9]]) as usize;
        let Some(data) = p.get(next + 10..next + 10 + len) else {
            return out;
        };
        if rtype == 1 && len == 4 && rname == name {
            let ip = Ipv4Addr::new(data[0], data[1], data[2], data[3]);
            if !out.contains(&ip) {
                out.push(ip);
            }
        }
        pos = next + 10 + len;
    }
    out
}

/// A socket in the mDNS group on port 5353, shared with the system's own
/// mDNS service, which hears multicast answers and announcements (Chrome
/// answers only this way).
fn group_socket(local_v4: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), MDNS_GROUP.port()).into())?;
    sock.join_multicast_v4(MDNS_GROUP.ip(), &local_v4)?;
    sock.set_multicast_if_v4(&local_v4)?;
    // Also hear a browser on this same computer.
    sock.set_multicast_loop_v4(true)?;
    sock.set_multicast_ttl_v4(255)?;
    sock.set_nonblocking(true)?;
    Ok(sock.into())
}

/// A socket on a free port for one-shot queries (answered straight back).
fn one_shot_socket(local_v4: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
    sock.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).into())?;
    // Out through the local network's interface, not a VPN's.
    sock.set_multicast_if_v4(&local_v4)?;
    sock.set_multicast_ttl_v4(255)?;
    sock.set_nonblocking(true)?;
    Ok(sock.into())
}

/// Resolves `names` on the local network reached through `local_ip` (the
/// interface this device's host candidate uses). Asks both ways: a
/// standard query from port 5353 answered by multicast, and a one-shot
/// query answered directly. Queries are repeated (Wi-Fi drops multicast
/// now and then, especially to and from sleeping devices) until every name
/// answered or `timeout` passed; `found` is called as each name resolves.
pub fn resolve(
    names: &[String],
    local_ip: IpAddr,
    timeout: Duration,
    mut found: impl FnMut(&str, Ipv4Addr),
) -> Result<(), String> {
    let IpAddr::V4(local_v4) = local_ip else {
        return Err("mDNS lookups need an IPv4 local address".into());
    };
    if local_v4.is_unspecified() || local_v4.is_loopback() {
        return Err("no local network address for mDNS lookups".into());
    }
    let group = group_socket(local_v4).ok();
    let one_shot = one_shot_socket(local_v4).ok();
    if group.is_none() && one_shot.is_none() {
        return Err("could not open an mDNS socket".into());
    }
    let mut pending: Vec<String> = names.to_vec();
    let start = Instant::now();
    // Ask at 0, 0.25, 0.75, 1.5, 2.5, 4, 6 and 8 seconds.
    let schedule = [0u64, 250, 750, 1500, 2500, 4000, 6000, 8000];
    let mut next_send = 0;
    let mut buf = [0u8; 1500];
    while !pending.is_empty() && start.elapsed() < timeout {
        if next_send < schedule.len()
            && start.elapsed() >= Duration::from_millis(schedule[next_send])
        {
            for n in &pending {
                if let Some(s) = &group {
                    let _ = s.send_to(&query_packet(n, false), MDNS_GROUP);
                }
                if let Some(s) = &one_shot {
                    let _ = s.send_to(&query_packet(n, true), MDNS_GROUP);
                }
            }
            next_send += 1;
        }
        let mut heard = false;
        for s in [&group, &one_shot].into_iter().flatten() {
            while let Ok((n, _from)) = s.recv_from(&mut buf) {
                heard = true;
                pending.retain(|name| match parse_answers(&buf[..n], name).first() {
                    Some(ip) => {
                        found(name, *ip);
                        false
                    }
                    None => true,
                });
            }
        }
        if !heard {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDP: &str = "v=0\r\n\
        a=candidate:1 1 udp 2122260223 4b1f9f7e-2c3a-4d5e-8f90-a1b2c3d4e5f6.local 54321 typ host generation 0\r\n\
        a=candidate:1 1 udp 2122260223 4b1f9f7e-2c3a-4d5e-8f90-a1b2c3d4e5f6.local 54321 typ host generation 0\r\n\
        a=candidate:2 1 udp 1686052607 203.0.113.7 54321 typ srflx raddr 0.0.0.0 rport 0\r\n\
        a=candidate:3 1 tcp 1518280447 other.local 9 typ host tcptype active\r\n\
        a=candidate:4 1 udp 2122260223 192.168.1.20 5000 typ host\r\n\
        a=candidate:5 1 udp 2122260223 bad_name!.local 5000 typ host\r\n";

    #[test]
    fn finds_only_udp_host_candidates_with_local_names_once() {
        assert_eq!(
            hidden_candidates(SDP),
            vec![HiddenCandidate {
                name: "4b1f9f7e-2c3a-4d5e-8f90-a1b2c3d4e5f6.local".into(),
                port: 54321
            }]
        );
    }

    #[test]
    fn query_asks_for_an_ipv4_answer_unicast_or_multicast() {
        assert_eq!(
            query_packet("ab.local", false).last_chunk::<4>(),
            Some(&[0, 1, 0, 1])
        );
        let q = query_packet("ab.local", true);
        assert_eq!(&q[..12], &[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            &q[12..],
            &[
                2, b'a', b'b', 5, b'l', b'o', b'c', b'a', b'l', 0, 0, 1, 0x80, 1
            ]
        );
    }

    /// The answer the Mac sent in a real capture: the question echoed,
    /// one A record (name compressed to the question), and an OPT record.
    #[test]
    fn parses_a_real_answer_with_compression_and_extra_records() {
        let mut p = vec![0, 0, 0x84, 0, 0, 1, 0, 1, 0, 0, 0, 1];
        p.extend_from_slice(&query_packet("mac.local", true)[12..]);
        p.extend_from_slice(&[
            0xC0, 12, 0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4, 192, 168, 1, 134,
        ]);
        p.extend_from_slice(&[0, 0, 41, 5, 0xA0, 0, 0, 0x11, 0x94, 0, 0]);
        assert_eq!(
            parse_answers(&p, "mac.local"),
            vec![Ipv4Addr::new(192, 168, 1, 134)]
        );
        assert!(parse_answers(&p, "other.local").is_empty());
    }

    #[test]
    fn ignores_queries_truncation_and_pointer_loops() {
        let query = query_packet("mac.local", true);
        assert!(parse_answers(&query, "mac.local").is_empty());
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        p.extend_from_slice(&[0xC0, 12]); // points at itself
        assert!(parse_answers(&p, "mac.local").is_empty());
        let mut short = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        short.extend_from_slice(&query_packet("mac.local", true)[12..20]);
        assert!(parse_answers(&short, "mac.local").is_empty());
    }
}
