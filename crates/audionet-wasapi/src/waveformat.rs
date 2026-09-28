//! Parsing of `WAVEFORMATEX` / `WAVEFORMATEXTENSIBLE` byte layouts.
//!
//! Windows reports endpoint formats as these packed little-endian structures,
//! both from `IAudioClient::GetMixFormat` and as the `VT_BLOB` value of
//! `PKEY_AudioEngine_DeviceFormat`. Parsing them from a byte slice keeps the
//! logic safe and testable without Windows.
//!
//! Layout (offsets in bytes):
//!
//! ```text
//! WAVEFORMATEX (18 bytes)
//!   0  u16 wFormatTag      2  u16 nChannels     4  u32 nSamplesPerSec
//!   8  u32 nAvgBytesPerSec 12 u16 nBlockAlign   14 u16 wBitsPerSample
//!   16 u16 cbSize          (bytes of extension that follow)
//! WAVEFORMATEXTENSIBLE extension (cbSize >= 22)
//!   18 u16 wValidBitsPerSample  20 u32 dwChannelMask  24 GUID SubFormat
//! ```

use core::fmt;

use audionet_protocol::{DeviceFormat, SampleEncoding, SampleFormat};

const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

const WAVEFORMATEX_LEN: usize = 18;
const EXTENSIBLE_EXTRA_LEN: usize = 22;

/// Bytes 4..16 shared by every `KSDATAFORMAT_SUBTYPE_*` GUID derived from a
/// WAVE format tag (`xxxxxxxx-0000-0010-8000-00aa00389b71`), in the GUID's
/// in-memory order: Data2 and Data3 little-endian, then Data4 as-is.
const KS_SUBTYPE_TAIL: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

/// Why a format blob could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaveFormatError {
    TooShort {
        len: usize,
        needed: usize,
    },
    /// A `WAVE_FORMAT_EXTENSIBLE` tag whose `cbSize` is too small for the extension.
    ExtensionTooShort {
        cb_size: usize,
    },
    ZeroChannels,
    ZeroSampleRate,
}

impl fmt::Display for WaveFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaveFormatError::TooShort { len, needed } => {
                write!(
                    f,
                    "format data is {len} bytes; at least {needed} are needed"
                )
            }
            WaveFormatError::ExtensionTooShort { cb_size } => write!(
                f,
                "extensible format declares {cb_size} extension bytes; {EXTENSIBLE_EXTRA_LEN} are needed"
            ),
            WaveFormatError::ZeroChannels => f.write_str("format reports zero channels"),
            WaveFormatError::ZeroSampleRate => f.write_str("format reports a zero sample rate"),
        }
    }
}

impl std::error::Error for WaveFormatError {}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn encoding_for_tag(tag: u16) -> SampleEncoding {
    match tag {
        WAVE_FORMAT_PCM => SampleEncoding::Integer,
        WAVE_FORMAT_IEEE_FLOAT => SampleEncoding::Float,
        _ => SampleEncoding::Other,
    }
}

/// Parses a `WAVEFORMATEX` or `WAVEFORMATEXTENSIBLE`.
///
/// `bytes` may be longer than the structure; trailing bytes are ignored.
pub fn parse_waveformat(bytes: &[u8]) -> Result<DeviceFormat, WaveFormatError> {
    if bytes.len() < WAVEFORMATEX_LEN {
        return Err(WaveFormatError::TooShort {
            len: bytes.len(),
            needed: WAVEFORMATEX_LEN,
        });
    }
    let tag = u16_at(bytes, 0);
    let channels = u16_at(bytes, 2);
    let sample_rate_hz = u32_at(bytes, 4);
    let bits = u16_at(bytes, 14);
    let cb_size = usize::from(u16_at(bytes, 16));

    if channels == 0 {
        return Err(WaveFormatError::ZeroChannels);
    }
    if sample_rate_hz == 0 {
        return Err(WaveFormatError::ZeroSampleRate);
    }

    let mut sample_format = SampleFormat {
        encoding: encoding_for_tag(tag),
        container_bits: bits,
        valid_bits: bits,
    };
    let mut channel_mask = None;

    if tag == WAVE_FORMAT_EXTENSIBLE {
        if cb_size < EXTENSIBLE_EXTRA_LEN {
            return Err(WaveFormatError::ExtensionTooShort { cb_size });
        }
        let needed = WAVEFORMATEX_LEN + EXTENSIBLE_EXTRA_LEN;
        if bytes.len() < needed {
            return Err(WaveFormatError::TooShort {
                len: bytes.len(),
                needed,
            });
        }
        let valid_bits = u16_at(bytes, 18);
        channel_mask = Some(u32_at(bytes, 20));
        let sub = &bytes[24..40];
        sample_format.encoding = if sub[4..] == KS_SUBTYPE_TAIL {
            // Data1 of a KS subtype GUID is the WAVE format tag.
            u16::try_from(u32_at(sub, 0)).map_or(SampleEncoding::Other, encoding_for_tag)
        } else {
            SampleEncoding::Other
        };
        // Some drivers leave wValidBitsPerSample at 0 meaning "all bits".
        if valid_bits != 0 {
            sample_format.valid_bits = valid_bits;
        }
    }

    Ok(DeviceFormat {
        sample_rate_hz,
        channels,
        sample_format,
        channel_mask,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waveformatex(tag: u16, channels: u16, rate: u32, bits: u16, cb_size: u16) -> Vec<u8> {
        let block_align = channels * bits / 8;
        let mut v = Vec::new();
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(&cb_size.to_le_bytes());
        v
    }

    fn extensible(
        channels: u16,
        rate: u32,
        bits: u16,
        valid: u16,
        mask: u32,
        sub_tag: u32,
    ) -> Vec<u8> {
        let mut v = waveformatex(WAVE_FORMAT_EXTENSIBLE, channels, rate, bits, 22);
        v.extend_from_slice(&valid.to_le_bytes());
        v.extend_from_slice(&mask.to_le_bytes());
        v.extend_from_slice(&sub_tag.to_le_bytes());
        v.extend_from_slice(&KS_SUBTYPE_TAIL);
        v
    }

    #[test]
    fn typical_shared_mode_mix_format() {
        // 48 kHz stereo float, the usual WASAPI shared-mode mix format.
        let f = parse_waveformat(&extensible(2, 48_000, 32, 32, 0x3, 3)).unwrap();
        assert_eq!(f.sample_rate_hz, 48_000);
        assert_eq!(f.channels, 2);
        assert_eq!(f.sample_format, SampleFormat::F32);
        assert_eq!(f.channel_mask, Some(0x3));
    }

    #[test]
    fn pcm_24_in_32() {
        let f = parse_waveformat(&extensible(8, 96_000, 32, 24, 0x63F, 1)).unwrap();
        assert_eq!(f.channels, 8);
        assert_eq!(
            f.sample_format,
            SampleFormat {
                encoding: SampleEncoding::Integer,
                container_bits: 32,
                valid_bits: 24
            }
        );
    }

    #[test]
    fn plain_waveformatex() {
        let f = parse_waveformat(&waveformatex(WAVE_FORMAT_PCM, 1, 44_100, 16, 0)).unwrap();
        assert_eq!(f.sample_format.encoding, SampleEncoding::Integer);
        assert_eq!(f.sample_format.valid_bits, 16);
        assert_eq!(f.channel_mask, None);
    }

    #[test]
    fn unknown_subformat_is_other() {
        let mut bytes = extensible(2, 48_000, 16, 16, 0x3, 0x92);
        bytes[30] ^= 0xFF; // corrupt the GUID tail: not a KS WAVE subtype
        let f = parse_waveformat(&bytes).unwrap();
        assert_eq!(f.sample_format.encoding, SampleEncoding::Other);
    }

    #[test]
    fn zero_valid_bits_means_container_bits() {
        let f = parse_waveformat(&extensible(2, 48_000, 16, 0, 0x3, 1)).unwrap();
        assert_eq!(f.sample_format.valid_bits, 16);
    }

    #[test]
    fn rejects_truncated_and_degenerate_input() {
        assert!(matches!(
            parse_waveformat(&[0; 10]),
            Err(WaveFormatError::TooShort { needed: 18, .. })
        ));
        let mut truncated = extensible(2, 48_000, 32, 32, 3, 3);
        truncated.truncate(30);
        assert!(matches!(
            parse_waveformat(&truncated),
            Err(WaveFormatError::TooShort { needed: 40, .. })
        ));
        assert_eq!(
            parse_waveformat(&waveformatex(WAVE_FORMAT_EXTENSIBLE, 2, 48_000, 16, 0)),
            Err(WaveFormatError::ExtensionTooShort { cb_size: 0 })
        );
        assert_eq!(
            parse_waveformat(&waveformatex(WAVE_FORMAT_PCM, 0, 48_000, 16, 0)),
            Err(WaveFormatError::ZeroChannels)
        );
        assert_eq!(
            parse_waveformat(&waveformatex(WAVE_FORMAT_PCM, 2, 0, 16, 0)),
            Err(WaveFormatError::ZeroSampleRate)
        );
    }
}
