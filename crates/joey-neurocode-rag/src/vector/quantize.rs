//! Vector BLOB encoding/decoding (f32 little-endian, symmetric int8).
//!
//! The single source of truth for the `rag_vectors.vector` BLOB layout
//! (contracts/rag-store-schema.md § BLOB encoding; data-model.md §2
//! VectorRecord; research.md R1):
//!
//! | Encoding | Layout | Byte length |
//! |---|---|---|
//! | `f32` | `dim` little-endian IEEE-754 words | `dim × 4` |
//! | `int8` | one f32 scale prefix (4 bytes LE) + `dim` int8 codes (value ≈ code × scale) | `dim × 1 + 4` |
//!
//! Anything whose byte length differs is corrupt and rejected. Vectors are
//! stored normalized upstream (the embedder L2-normalizes); cosine is then a
//! plain dot product on decode — de/quantization here is lossy only for int8.
//!
//! T012 (this file) owns the helpers; the exhaustive scan (T013,
//! `vector::scan.rs`) imports them from here / via `vector::store`
//! re-exports — never re-implements the layout.

use std::fmt;

/// Storage encoding of a vector BLOB (`rag_vectors.quantization`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quantization {
    /// Raw little-endian f32 words, `dim × 4` bytes.
    F32,
    /// Symmetric per-vector int8: 4-byte LE f32 scale prefix + `dim` codes,
    /// `dim + 4` bytes (value ≈ code × scale).
    Int8,
}

impl Quantization {
    /// The persisted discriminator (`'f32'` / `'int8'`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Quantization::F32 => "f32",
            Quantization::Int8 => "int8",
        }
    }

    /// Parse the persisted discriminator (case-sensitive, per the DDL CHECK).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "f32" => Some(Quantization::F32),
            "int8" => Some(Quantization::Int8),
            _ => None,
        }
    }

    /// The exact BLOB byte length for `dim` elements under this encoding.
    pub const fn blob_len(self, dim: usize) -> usize {
        match self {
            Quantization::F32 => dim * 4,
            Quantization::Int8 => dim + 4,
        }
    }
}

/// A BLOB that is not the exact byte length its encoding demands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantizeError {
    pub encoding: &'static str,
    pub expected_len: usize,
    pub got_len: usize,
}

impl fmt::Display for QuantizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "corrupt {} vector BLOB: expected {} bytes, got {}",
            self.encoding, self.expected_len, self.got_len
        )
    }
}

impl std::error::Error for QuantizeError {}

fn check_len(bytes: &[u8], dim: usize, q: Quantization) -> Result<(), QuantizeError> {
    let expected = q.blob_len(dim);
    if bytes.len() == expected {
        Ok(())
    } else {
        Err(QuantizeError { encoding: q.as_str(), expected_len: expected, got_len: bytes.len() })
    }
}

/// Encode `v` as a little-endian f32 BLOB (`dim × 4` bytes).
pub fn encode_f32(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decode a little-endian f32 BLOB of exactly `dim` elements.
pub fn decode_f32(bytes: &[u8], dim: usize) -> Result<Vec<f32>, QuantizeError> {
    check_len(bytes, dim, Quantization::F32)?;
    Ok(bytes
        .chunks_exact(4)
        .map(|w| f32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect())
}

/// int8 code range: symmetric quantization maps `[-scale, +scale]` onto
/// `[-127, 127]` (avoiding the asymmetric -128 keeps decode a clean `code ×
/// scale` multiply).
const INT8_MAX: f32 = 127.0;

/// Encode `v` as a symmetric int8 BLOB: 4-byte LE f32 scale prefix followed
/// by `dim` int8 codes (`dim + 4` bytes). `scale = max|v| / 127`; a zero
/// vector encodes with scale `0.0` and all-zero codes.
pub fn encode_int8(v: &[f32]) -> Vec<u8> {
    let max_abs = v.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    let scale = max_abs / INT8_MAX;
    let mut out = Vec::with_capacity(v.len() + 4);
    out.extend_from_slice(&scale.to_le_bytes());
    if scale == 0.0 {
        out.resize(v.len() + 4, 0);
        return out;
    }
    for &x in v {
        let code = (x / scale).round().clamp(-INT8_MAX, INT8_MAX) as i8;
        out.push(code as u8);
    }
    out
}

/// Decode a symmetric int8 BLOB of exactly `dim` codes (4-byte LE f32 scale
/// prefix + `dim` codes): `value = code × scale`.
pub fn decode_int8(bytes: &[u8], dim: usize) -> Result<Vec<f32>, QuantizeError> {
    check_len(bytes, dim, Quantization::Int8)?;
    let scale = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    Ok(bytes[4..]
        .iter()
        .map(|&c| c as i8 as f32 * scale)
        .collect())
}

/// Encode under either encoding (dispatch helper).
pub fn encode(v: &[f32], q: Quantization) -> Vec<u8> {
    match q {
        Quantization::F32 => encode_f32(v),
        Quantization::Int8 => encode_int8(v),
    }
}

/// Decode under either encoding, enforcing the exact BLOB length for `dim`.
pub fn decode(bytes: &[u8], dim: usize, q: Quantization) -> Result<Vec<f32>, QuantizeError> {
    match q {
        Quantization::F32 => decode_f32(bytes, dim),
        Quantization::Int8 => decode_int8(bytes, dim),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_layout_is_dim_times_4_little_endian() {
        let v = vec![1.0f32, -2.5, 3.25, 0.0];
        let b = encode_f32(&v);
        assert_eq!(b.len(), v.len() * 4);
        // First element is the LE IEEE-754 word for 1.0: 00 00 80 3F.
        assert_eq!(&b[..4], &[0x00, 0x00, 0x80, 0x3F]);
        // -2.5 = 0xC0200000 → LE bytes 00 00 20 C0.
        assert_eq!(&b[4..8], &[0x00, 0x00, 0x20, 0xC0]);
    }

    #[test]
    fn f32_round_trip_exact() {
        let v: Vec<f32> = (0..64).map(|i| (i as f32) * 0.25 - 8.0).collect();
        let decoded = decode_f32(&encode_f32(&v), v.len()).unwrap();
        assert_eq!(decoded, v); // byte-exact, no loss
    }

    #[test]
    fn int8_layout_is_dim_plus_4_with_scale_prefix() {
        let v = vec![1.0f32, -0.5, 0.25];
        let b = encode_int8(&v);
        assert_eq!(b.len(), v.len() + 4);
        let scale = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        assert!((scale - 1.0 / 127.0).abs() < 1e-9); // max|v|=1 → scale = 1/127
        let codes: Vec<i8> = b[4..].iter().map(|&c| c as i8).collect();
        assert_eq!(codes[0], 127); // max element quantizes to full scale
    }

    #[test]
    fn int8_round_trip_within_half_scale() {
        let v: Vec<f32> = (0..100).map(|i| ((i % 17) as f32) * 0.13 - 1.1).collect();
        let b = encode_int8(&v);
        let scale = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let decoded = decode_int8(&b, v.len()).unwrap();
        for (orig, got) in v.iter().zip(decoded.iter()) {
            assert!((orig - got).abs() <= scale / 2.0 + 1e-6, "{orig} vs {got}");
        }
    }

    #[test]
    fn int8_zero_vector_is_all_zero() {
        let v = vec![0.0f32; 16];
        let b = encode_int8(&v);
        assert_eq!(b.len(), 20);
        assert!(b.iter().all(|&x| x == 0));
        assert!(decode_int8(&b, 16).unwrap().iter().all(|&x| x == 0.0));
    }

    #[test]
    fn wrong_length_rejected_both_encodings() {
        let err = decode_f32(&[0u8; 7], 2).unwrap_err();
        assert_eq!(
            err.to_string(),
            "corrupt f32 vector BLOB: expected 8 bytes, got 7"
        );
        let err = decode_int8(&[0u8; 9], 8).unwrap_err(); // needs 12
        assert_eq!(err.expected_len, 12);
        assert_eq!(err.got_len, 9);
        assert_eq!(Quantization::F32.blob_len(768), 768 * 4);
        assert_eq!(Quantization::Int8.blob_len(768), 768 + 4);
    }

    #[test]
    fn dispatch_round_trips() {
        let v: Vec<f32> = (0..32).map(|i| (i as f32 - 16.0) / 16.0).collect();
        for q in [Quantization::F32, Quantization::Int8] {
            let b = encode(&v, q);
            assert_eq!(b.len(), q.blob_len(v.len()));
            let d = decode(&b, v.len(), q).unwrap();
            match q {
                Quantization::F32 => assert_eq!(d, v),
                Quantization::Int8 => {
                    let err = decode(&b, v.len() + 1, q).unwrap_err();
                    assert_eq!(err.got_len, b.len());
                }
            }
        }
    }

    #[test]
    fn discriminator_round_trip() {
        assert_eq!(Quantization::parse("f32"), Some(Quantization::F32));
        assert_eq!(Quantization::parse("int8"), Some(Quantization::Int8));
        assert_eq!(Quantization::parse("float32"), None);
        assert_eq!(Quantization::F32.as_str(), "f32");
        assert_eq!(Quantization::Int8.as_str(), "int8");
    }
}
