//! RTP fixed header (RFC 3550 §5.1).
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! |V=2|P|X|  CC   |M|     PT      |       sequence number         |
//! |                           timestamp                           |
//! |           synchronization source (SSRC) identifier            |
//! ```
//!
//! AudioNet writes the 12-byte fixed header with no CSRCs, extension or
//! padding. The parser accepts CSRCs and header extensions (skipping them)
//! so packets from standard RTP stacks parse too.

use core::fmt;

/// Dynamic payload type AudioNet uses for Opus (the WebRTC convention).
pub const PT_OPUS: u8 = 111;

pub const HEADER_LEN: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    /// Media timestamp in samples at the codec clock (48 kHz for Opus).
    pub timestamp: u32,
    pub ssrc: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RtpError {
    TooShort,
    BadVersion(u8),
    BadPadding,
}

impl fmt::Display for RtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtpError::TooShort => f.write_str("packet is too short for its RTP header"),
            RtpError::BadVersion(v) => write!(f, "RTP version {v} is not 2"),
            RtpError::BadPadding => f.write_str("RTP padding length is invalid"),
        }
    }
}

impl std::error::Error for RtpError {}

impl RtpHeader {
    /// Writes the 12-byte header into `out[..12]`.
    pub fn write(&self, out: &mut [u8]) {
        out[0] = 0x80; // V=2, P=0, X=0, CC=0
        out[1] = (u8::from(self.marker) << 7) | (self.payload_type & 0x7F);
        out[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        out[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        out[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
    }

    /// Parses a packet. Returns the header and the byte range of the
    /// payload (after CSRCs and extension, before padding).
    pub fn parse(packet: &[u8]) -> Result<(RtpHeader, core::ops::Range<usize>), RtpError> {
        if packet.len() < HEADER_LEN {
            return Err(RtpError::TooShort);
        }
        let version = packet[0] >> 6;
        if version != 2 {
            return Err(RtpError::BadVersion(version));
        }
        let padding = packet[0] & 0x20 != 0;
        let extension = packet[0] & 0x10 != 0;
        let csrc_count = usize::from(packet[0] & 0x0F);
        let mut start = HEADER_LEN + 4 * csrc_count;
        if extension {
            if packet.len() < start + 4 {
                return Err(RtpError::TooShort);
            }
            let words = usize::from(u16::from_be_bytes([packet[start + 2], packet[start + 3]]));
            start += 4 + 4 * words;
        }
        let mut end = packet.len();
        if padding {
            let pad = usize::from(*packet.last().ok_or(RtpError::TooShort)?);
            if pad == 0 || pad > end.saturating_sub(start) {
                return Err(RtpError::BadPadding);
            }
            end -= pad;
        }
        if start > end {
            return Err(RtpError::TooShort);
        }
        let header = RtpHeader {
            marker: packet[1] & 0x80 != 0,
            payload_type: packet[1] & 0x7F,
            sequence: u16::from_be_bytes([packet[2], packet[3]]),
            timestamp: u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
            ssrc: u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]),
        };
        Ok((header, start..end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> RtpHeader {
        RtpHeader {
            marker: true,
            payload_type: PT_OPUS,
            sequence: 0xFFFE,
            timestamp: 0xDEAD_BEEF,
            ssrc: 0x1234_5678,
        }
    }

    #[test]
    fn round_trip() {
        let mut buf = [0u8; 16];
        header().write(&mut buf);
        buf[12..].copy_from_slice(b"opus");
        let (h, payload) = RtpHeader::parse(&buf).unwrap();
        assert_eq!(h, header());
        assert_eq!(&buf[payload], b"opus");
        assert_eq!(buf[0], 0x80);
        assert_eq!(buf[1], 0x80 | 111);
    }

    #[test]
    fn skips_csrcs_and_extension_and_padding() {
        let mut p = vec![0x80 | 0x20 | 0x10 | 0x01, 111, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
        p.extend_from_slice(&[9, 9, 9, 9]); // one CSRC
        p.extend_from_slice(&[0xBE, 0xDE, 0, 1, 1, 2, 3, 4]); // extension, 1 word
        p.extend_from_slice(b"pay");
        p.extend_from_slice(&[0, 2]); // 2 bytes of padding
        let (h, range) = RtpHeader::parse(&p).unwrap();
        assert_eq!(h.sequence, 1);
        assert_eq!(&p[range], b"pay");
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(RtpHeader::parse(&[0x80; 5]), Err(RtpError::TooShort));
        let mut bad = [0u8; 12];
        bad[0] = 0x40;
        assert_eq!(RtpHeader::parse(&bad), Err(RtpError::BadVersion(1)));
        let mut pad = [0u8; 13];
        pad[0] = 0xA0;
        pad[12] = 9;
        assert_eq!(RtpHeader::parse(&pad), Err(RtpError::BadPadding));
    }
}
