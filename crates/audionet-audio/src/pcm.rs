//! Conversion of device PCM layouts to interleaved `f32`.
//!
//! Conversion runs on capture threads, so it writes into a caller-provided
//! slice and never allocates. The layout is chosen once, on the control
//! path, when a stream opens.

use core::fmt;

use audionet_protocol::{DeviceFormat, SampleEncoding};

/// A device sample layout AudioNet can convert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmLayout {
    /// 32-bit IEEE float (the usual WASAPI shared-mode mix format).
    F32,
    /// 16-bit signed integer.
    I16,
    /// 24-bit signed integer packed in 3 bytes.
    I24Packed,
    /// Signed integer in a 32-bit container (valid bits 17–32, left-justified).
    I32,
}

/// The device format cannot be converted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedPcm {
    pub description: String,
}

impl fmt::Display for UnsupportedPcm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported sample format: {}", self.description)
    }
}

impl std::error::Error for UnsupportedPcm {}

impl PcmLayout {
    pub fn from_format(format: &DeviceFormat) -> Result<Self, UnsupportedPcm> {
        let sf = format.sample_format;
        let layout = match (sf.encoding, sf.container_bits) {
            (SampleEncoding::Float, 32) => Some(PcmLayout::F32),
            (SampleEncoding::Integer, 16) => Some(PcmLayout::I16),
            (SampleEncoding::Integer, 24) => Some(PcmLayout::I24Packed),
            (SampleEncoding::Integer, 32) => Some(PcmLayout::I32),
            _ => None,
        };
        layout.ok_or_else(|| UnsupportedPcm {
            description: sf.describe(),
        })
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self {
            PcmLayout::F32 | PcmLayout::I32 => 4,
            PcmLayout::I16 => 2,
            PcmLayout::I24Packed => 3,
        }
    }

    /// Converts `out.len()` samples from the start of `bytes` into `out`.
    ///
    /// # Panics
    ///
    /// If `bytes` holds fewer than `out.len()` samples. Callers size both
    /// from the same frame count.
    pub fn convert(self, bytes: &[u8], out: &mut [f32]) {
        let n = out.len();
        let bytes = &bytes[..n * self.bytes_per_sample()];
        match self {
            PcmLayout::F32 => {
                for (o, b) in out.iter_mut().zip(bytes.chunks_exact(4)) {
                    *o = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                }
            }
            PcmLayout::I16 => {
                for (o, b) in out.iter_mut().zip(bytes.chunks_exact(2)) {
                    *o = f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0;
                }
            }
            PcmLayout::I24Packed => {
                for (o, b) in out.iter_mut().zip(bytes.chunks_exact(3)) {
                    // Place the 24 bits at the top of an i32 to sign-extend.
                    let v = i32::from_le_bytes([0, b[0], b[1], b[2]]);
                    *o = (v as f64 / 2_147_483_648.0) as f32;
                }
            }
            PcmLayout::I32 => {
                for (o, b) in out.iter_mut().zip(bytes.chunks_exact(4)) {
                    let v = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    *o = (v as f64 / 2_147_483_648.0) as f32;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audionet_protocol::SampleFormat;

    fn format(encoding: SampleEncoding, container_bits: u16, valid_bits: u16) -> DeviceFormat {
        DeviceFormat {
            sample_rate_hz: 48_000,
            channels: 2,
            sample_format: SampleFormat {
                encoding,
                container_bits,
                valid_bits,
            },
            channel_mask: None,
        }
    }

    #[test]
    fn selects_layouts() {
        use SampleEncoding::*;
        assert_eq!(
            PcmLayout::from_format(&format(Float, 32, 32)),
            Ok(PcmLayout::F32)
        );
        assert_eq!(
            PcmLayout::from_format(&format(Integer, 16, 16)),
            Ok(PcmLayout::I16)
        );
        assert_eq!(
            PcmLayout::from_format(&format(Integer, 24, 24)),
            Ok(PcmLayout::I24Packed)
        );
        assert_eq!(
            PcmLayout::from_format(&format(Integer, 32, 24)),
            Ok(PcmLayout::I32)
        );
        assert!(PcmLayout::from_format(&format(Float, 64, 64)).is_err());
        assert!(PcmLayout::from_format(&format(Other, 16, 16)).is_err());
    }

    #[test]
    fn converts_f32_exactly() {
        let src = [0.5f32, -0.25, 1.0];
        let bytes: Vec<u8> = src.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut out = [0.0; 3];
        PcmLayout::F32.convert(&bytes, &mut out);
        assert_eq!(out, src);
    }

    #[test]
    fn converts_integer_extremes() {
        let bytes: Vec<u8> = [i16::MIN, 0, i16::MAX, 16_384]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut out = [0.0; 4];
        PcmLayout::I16.convert(&bytes, &mut out);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[1], 0.0);
        assert!((out[2] - 1.0).abs() < 1e-4);
        assert_eq!(out[3], 0.5);

        // 24-bit packed: -2^23, +2^22 (0.5).
        let bytes = [0x00, 0x00, 0x80, 0x00, 0x00, 0x40];
        let mut out = [0.0; 2];
        PcmLayout::I24Packed.convert(&bytes, &mut out);
        assert_eq!(out, [-1.0, 0.5]);

        // 24 valid bits left-justified in 32: 0.5 is 0x4000_0000.
        let bytes: Vec<u8> = [0x4000_0000i32, i32::MIN]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut out = [0.0; 2];
        PcmLayout::I32.convert(&bytes, &mut out);
        assert_eq!(out, [0.5, -1.0]);
    }

    #[test]
    fn converts_only_requested_count() {
        let bytes = [0u8; 16];
        let mut out = [9.0f32; 2];
        PcmLayout::I16.convert(&bytes, &mut out);
        assert_eq!(out, [0.0, 0.0]);
    }
}
