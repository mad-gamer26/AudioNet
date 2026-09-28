//! Per-packet authenticated encryption for pre-shared-key (LAN) mode.
//!
//! Wire format of an encrypted media packet:
//!
//! ```text
//! +----------------------+----------------+---------------------------+----------+
//! | RTP header (12 bytes)| nonce (24 B)   | encrypted Opus payload    | tag (16) |
//! +----------------------+----------------+---------------------------+----------+
//!   authenticated (AAD)    random per packet  XChaCha20                  Poly1305
//! ```
//!
//! * **Cipher:** XChaCha20-Poly1305 (RustCrypto). Its 192-bit nonce is safe
//!   to choose at random for every packet, so there is no nonce state to
//!   lose on restart and no risk of reuse across sessions sharing a key.
//! * **Key:** derived from the 32-byte pre-shared key with HKDF-SHA256 and a
//!   fixed context string, so the raw PSK is never used directly and future
//!   key types can be separated by context.
//! * **Header:** the RTP header stays in the clear (so it can be inspected
//!   and routed) but is authenticated; tampering with sequence numbers,
//!   timestamps or SSRC makes the packet fail to open.
//! * **Replays:** a replayed packet authenticates, so the receiver drops it
//!   by sequence number (duplicate window). Replays of an *old session*
//!   (different SSRC) are a known limitation of static-PSK mode; the
//!   coordination-server path replaces the PSK with per-session keys.
//!
//! This is deliberately SRTP-shaped (clear header, encrypted payload, tag)
//! so the move to DTLS-SRTP in WebRTC does not change the media format.

use core::fmt;

use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::rtp::{HEADER_LEN, RtpError, RtpHeader};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
/// Bytes added to each packet beyond the RTP header and payload.
pub const OVERHEAD: usize = NONCE_LEN + TAG_LEN;

const KDF_SALT: &[u8] = b"AudioNet PSK v1";
const KDF_INFO_MEDIA: &[u8] = b"media packet key";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// The key file or string is not 32 bytes of hex.
    BadKey(String),
    /// The packet is malformed.
    Malformed(RtpError),
    /// Authentication failed: wrong key, corruption, or tampering.
    Authentication,
    /// The output buffer is too small.
    BufferTooSmall,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CryptoError::BadKey(why) => write!(f, "the pre-shared key is invalid: {why}"),
            CryptoError::Malformed(e) => write!(f, "malformed media packet: {e}"),
            CryptoError::Authentication => f.write_str(
                "media packet failed authentication (wrong key, corruption, or tampering)",
            ),
            CryptoError::BufferTooSmall => f.write_str("packet buffer is too small"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// A pre-shared key. Never printed.
#[derive(Clone)]
pub struct PresharedKey([u8; KEY_LEN]);

impl fmt::Debug for PresharedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PresharedKey(<redacted>)")
    }
}

impl PresharedKey {
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Generates a new random key from the OS-seeded CSPRNG.
    pub fn generate() -> Self {
        let mut k = [0u8; KEY_LEN];
        rand::fill(&mut k);
        Self(k)
    }

    /// Parses 64 hex digits (whitespace around them is ignored).
    pub fn from_hex(text: &str) -> Result<Self, CryptoError> {
        let t = text.trim();
        if t.len() != KEY_LEN * 2 {
            return Err(CryptoError::BadKey(format!(
                "expected {} hexadecimal digits, found {}",
                KEY_LEN * 2,
                t.len()
            )));
        }
        let mut k = [0u8; KEY_LEN];
        for (i, byte) in k.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&t[2 * i..2 * i + 2], 16)
                .map_err(|_| CryptoError::BadKey("contains a non-hexadecimal character".into()))?;
        }
        Ok(Self(k))
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Encrypts and decrypts media packets with a key derived from a PSK.
pub struct MediaCipher {
    aead: XChaCha20Poly1305,
}

impl fmt::Debug for MediaCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaCipher(<keyed>)")
    }
}

impl MediaCipher {
    /// Derives the media key. Control-path work: call once per session.
    pub fn new(psk: &PresharedKey) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(KDF_SALT), &psk.0);
        let mut key = [0u8; KEY_LEN];
        hk.expand(KDF_INFO_MEDIA, &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        let aead = XChaCha20Poly1305::new_from_slice(&key).expect("32-byte key");
        Self { aead }
    }

    /// Builds an encrypted packet into `out`: header, random nonce, then the
    /// encrypted payload and tag. Returns the packet length. Does not
    /// allocate.
    pub fn seal(
        &self,
        header: &RtpHeader,
        payload: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        let total = HEADER_LEN + NONCE_LEN + payload.len() + TAG_LEN;
        if out.len() < total {
            return Err(CryptoError::BufferTooSmall);
        }
        header.write(&mut out[..HEADER_LEN]);
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::fill(&mut nonce_bytes);
        out[HEADER_LEN..HEADER_LEN + NONCE_LEN].copy_from_slice(&nonce_bytes);
        let body_start = HEADER_LEN + NONCE_LEN;
        let body_end = body_start + payload.len();
        out[body_start..body_end].copy_from_slice(payload);
        let (aad, rest) = out.split_at_mut(HEADER_LEN);
        let body = &mut rest[NONCE_LEN..NONCE_LEN + payload.len()];
        let nonce = XNonce::from(nonce_bytes);
        let tag = self
            .aead
            .encrypt_inout_detached(&nonce, aad, body.into())
            .map_err(|_| CryptoError::BufferTooSmall)?;
        out[body_end..total].copy_from_slice(&tag);
        Ok(total)
    }

    /// Authenticates and decrypts a packet in place. Returns the header and
    /// the payload range within `packet`.
    pub fn open(
        &self,
        packet: &mut [u8],
    ) -> Result<(RtpHeader, core::ops::Range<usize>), CryptoError> {
        if packet.len() < HEADER_LEN + OVERHEAD {
            return Err(CryptoError::Malformed(RtpError::TooShort));
        }
        let (header, _) =
            RtpHeader::parse(&packet[..HEADER_LEN]).map_err(CryptoError::Malformed)?;
        let tag_start = packet.len() - TAG_LEN;
        let tag = Tag::try_from(&packet[tag_start..]).map_err(|_| CryptoError::Authentication)?;
        let nonce = XNonce::try_from(&packet[HEADER_LEN..HEADER_LEN + NONCE_LEN])
            .map_err(|_| CryptoError::Authentication)?;
        let (aad, rest) = packet.split_at_mut(HEADER_LEN);
        let body = &mut rest[NONCE_LEN..tag_start - HEADER_LEN];
        self.aead
            .decrypt_inout_detached(&nonce, aad, body.into(), &tag)
            .map_err(|_| CryptoError::Authentication)?;
        Ok((header, HEADER_LEN + NONCE_LEN..tag_start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp::PT_OPUS;

    fn header(seq: u16) -> RtpHeader {
        RtpHeader {
            marker: false,
            payload_type: PT_OPUS,
            sequence: seq,
            timestamp: u32::from(seq) * 480,
            ssrc: 42,
        }
    }

    #[test]
    fn seal_open_round_trip() {
        let cipher = MediaCipher::new(&PresharedKey::generate());
        let mut buf = [0u8; 256];
        let n = cipher.seal(&header(7), b"opus payload", &mut buf).unwrap();
        assert_eq!(n, 12 + OVERHEAD + 12);
        assert_ne!(&buf[36..48], b"opus payload", "payload is encrypted");
        let (h, range) = cipher.open(&mut buf[..n]).unwrap();
        assert_eq!(h, header(7));
        assert_eq!(&buf[range], b"opus payload");
    }

    #[test]
    fn nonces_differ_per_packet() {
        let cipher = MediaCipher::new(&PresharedKey::generate());
        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        cipher.seal(&header(1), b"x", &mut a).unwrap();
        cipher.seal(&header(1), b"x", &mut b).unwrap();
        assert_ne!(a[12..36], b[12..36]);
    }

    #[test]
    fn rejects_wrong_key_and_tampering() {
        let key = PresharedKey::generate();
        let cipher = MediaCipher::new(&key);
        let mut buf = [0u8; 64];
        let n = cipher.seal(&header(1), b"hello", &mut buf).unwrap();

        let other = MediaCipher::new(&PresharedKey::generate());
        assert_eq!(
            other.open(&mut { buf }[..n]).unwrap_err(),
            CryptoError::Authentication
        );

        for index in [3usize, 20, 40, n - 1] {
            // header (seq), nonce, body, tag
            let mut t = buf;
            t[index] ^= 1;
            assert_eq!(
                cipher.open(&mut t[..n]).unwrap_err(),
                CryptoError::Authentication,
                "flip at {index}"
            );
        }
        assert!(matches!(
            cipher.open(&mut buf[..20]),
            Err(CryptoError::Malformed(_))
        ));
    }

    #[test]
    fn hex_keys() {
        let k = PresharedKey::generate();
        let parsed = PresharedKey::from_hex(&format!("  {}\n", k.to_hex())).unwrap();
        assert_eq!(parsed.0, k.0);
        assert!(PresharedKey::from_hex("abc").is_err());
        assert!(PresharedKey::from_hex(&"zz".repeat(32)).is_err());
        assert_eq!(format!("{k:?}"), "PresharedKey(<redacted>)");
    }

    // 128 kbit/s at 10 ms is ~160 bytes of Opus; even the codec's maximum
    // packet stays under the datagram limit.
    const _: () = assert!(HEADER_LEN + OVERHEAD + 1000 <= crate::MAX_DATAGRAM);
}
