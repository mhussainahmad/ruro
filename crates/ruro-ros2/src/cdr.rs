//! CDR encoding as ROS 2 uses it: XCDR version 1, little endian (`CDR_LE`).
//!
//! A serialized message starts with the 4-byte encapsulation header
//! `00 01 00 00`. Each primitive is aligned to its own size (up to 8),
//! measured from the end of that header. Strings are a `u32` length that
//! counts a trailing NUL, the bytes, then the NUL. Sequences are a `u32`
//! element count followed by the elements; fixed-size arrays have no count.
//! Nested messages are their fields in order with no padding of their own.

use thiserror::Error;

/// The encapsulation header for little-endian plain CDR.
pub const CDR_LE: [u8; 4] = [0x00, 0x01, 0x00, 0x00];
/// The encapsulation header for big-endian plain CDR.
pub const CDR_BE: [u8; 4] = [0x00, 0x00, 0x00, 0x00];

/// Why bytes could not be decoded.
#[derive(Debug, Error, PartialEq)]
pub enum CdrError {
    #[error("message truncated at byte {0}")]
    Truncated(usize),
    #[error("unsupported encapsulation {0:02x?} (only little-endian CDR is supported)")]
    Encapsulation([u8; 4]),
    #[error("string is not NUL-terminated UTF-8")]
    BadString,
    #[error("sequence of {0} elements exceeds the remaining {1} bytes")]
    BadLength(usize, usize),
    #[error("{0}")]
    Invalid(String),
}

/// Writes CDR into a growing buffer.
pub struct CdrWriter {
    buf: Vec<u8>,
}

impl Default for CdrWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl CdrWriter {
    /// A writer that has already written the encapsulation header.
    pub fn new() -> Self {
        Self::with_capacity(64)
    }

    /// Like [`new`](Self::new) with room for `n` payload bytes.
    pub fn with_capacity(n: usize) -> Self {
        let mut buf = Vec::with_capacity(n + 4);
        buf.extend_from_slice(&CDR_LE);
        Self { buf }
    }

    /// Pads with zeros to a multiple of `n` from the payload start.
    pub fn align(&mut self, n: usize) {
        let pad = (n - (self.buf.len() - 4) % n) % n;
        self.buf.resize(self.buf.len() + pad, 0);
    }

    /// Writes raw bytes with no alignment.
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// Writes a sequence length.
    pub fn len(&mut self, n: usize) {
        self.u32(n as u32);
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    pub fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }

    pub fn u16(&mut self, v: u16) {
        self.align(2);
        self.bytes(&v.to_le_bytes());
    }

    pub fn i16(&mut self, v: i16) {
        self.align(2);
        self.bytes(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.align(4);
        self.bytes(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.align(4);
        self.bytes(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.align(8);
        self.bytes(&v.to_le_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.align(8);
        self.bytes(&v.to_le_bytes());
    }

    pub fn f32(&mut self, v: f32) {
        self.align(4);
        self.bytes(&v.to_le_bytes());
    }

    pub fn f64(&mut self, v: f64) {
        self.align(8);
        self.bytes(&v.to_le_bytes());
    }

    pub fn string(&mut self, s: &str) {
        self.len(s.len() + 1);
        self.bytes(s.as_bytes());
        self.buf.push(0);
    }

    /// A `wstring`: a count of UTF-16 code units, each written as 4 bytes,
    /// with no terminator, as ROS 2 Jazzy's Fast-CDR serializer writes it.
    pub fn wstring(&mut self, s: &str) {
        self.len(s.encode_utf16().count());
        for u in s.encode_utf16() {
            self.u32(u32::from(u));
        }
    }

    /// A `uint8[]`/`byte[]` sequence in one copy.
    pub fn byte_seq(&mut self, b: &[u8]) {
        self.len(b.len());
        self.bytes(b);
    }

    /// The encoded message, header included.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// Reads CDR from a byte slice.
pub struct CdrReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> CdrReader<'a> {
    /// Checks the encapsulation header and positions after it.
    pub fn new(data: &'a [u8]) -> Result<Self, CdrError> {
        if data.len() < 4 {
            return Err(CdrError::Truncated(data.len()));
        }
        let header: [u8; 4] = data[..4].try_into().unwrap();
        // Byte 1 selects the representation; bytes 2-3 are options.
        if header[0] != 0 || header[1] != CDR_LE[1] {
            return Err(CdrError::Encapsulation(header));
        }
        Ok(Self { data, pos: 4 })
    }

    /// Bytes not yet read.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn align(&mut self, n: usize) -> Result<(), CdrError> {
        let pad = (n - (self.pos - 4) % n) % n;
        if self.remaining() < pad {
            return Err(CdrError::Truncated(self.pos));
        }
        self.pos += pad;
        Ok(())
    }

    /// Takes `n` raw bytes.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], CdrError> {
        if self.remaining() < n {
            return Err(CdrError::Truncated(self.pos));
        }
        let b = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(b)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CdrError> {
        self.align(N)?;
        Ok(self.bytes(N)?.try_into().unwrap())
    }

    /// Reads a sequence length and checks that at least `min_elem` bytes per
    /// element remain, so a corrupt length cannot trigger a huge allocation.
    pub fn len(&mut self, min_elem: usize) -> Result<usize, CdrError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_elem) > self.remaining() {
            return Err(CdrError::BadLength(n, self.remaining()));
        }
        Ok(n)
    }

    pub fn u8(&mut self) -> Result<u8, CdrError> {
        Ok(self.bytes(1)?[0])
    }

    pub fn i8(&mut self) -> Result<i8, CdrError> {
        Ok(self.bytes(1)?[0] as i8)
    }

    pub fn bool(&mut self) -> Result<bool, CdrError> {
        Ok(self.bytes(1)?[0] != 0)
    }

    pub fn u16(&mut self) -> Result<u16, CdrError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn i16(&mut self) -> Result<i16, CdrError> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32, CdrError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32, CdrError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64, CdrError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Result<i64, CdrError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Result<f32, CdrError> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Result<f64, CdrError> {
        Ok(f64::from_le_bytes(self.array()?))
    }

    pub fn string(&mut self) -> Result<String, CdrError> {
        let n = self.len(1)?;
        if n == 0 {
            // Some writers send an empty string as length 0.
            return Ok(String::new());
        }
        let b = self.bytes(n)?;
        let (text, nul) = b.split_at(n - 1);
        if nul != [0] {
            return Err(CdrError::BadString);
        }
        String::from_utf8(text.to_vec()).map_err(|_| CdrError::BadString)
    }

    /// A `wstring` (see [`CdrWriter::wstring`]).
    pub fn wstring(&mut self) -> Result<String, CdrError> {
        let n = self.len(4)?;
        let mut units = Vec::with_capacity(n);
        for _ in 0..n {
            let u = self.u32()?;
            units
                .push(u16::try_from(u).map_err(|_| {
                    CdrError::Invalid(format!("wstring unit {u:#x} is not UTF-16"))
                })?);
        }
        String::from_utf16(&units)
            .map_err(|_| CdrError::Invalid("wstring is not valid UTF-16".into()))
    }

    /// A `uint8[]`/`byte[]` sequence in one copy.
    pub fn byte_seq(&mut self) -> Result<Vec<u8>, CdrError> {
        let n = self.len(1)?;
        Ok(self.bytes(n)?.to_vec())
    }
}

/// Types that can be written as CDR.
pub trait CdrEncode {
    fn encode(&self, w: &mut CdrWriter);
}

/// Types that can be read from CDR.
pub trait CdrDecode: Sized {
    fn decode(r: &mut CdrReader<'_>) -> Result<Self, CdrError>;

    /// Smallest encoded size of one value, used to sanity-check sequence lengths.
    const MIN_SIZE: usize = 1;
}

macro_rules! primitive {
    ($($t:ty => $m:ident, $size:expr);* $(;)?) => {$(
        impl CdrEncode for $t {
            fn encode(&self, w: &mut CdrWriter) {
                w.$m(*self)
            }
        }
        impl CdrDecode for $t {
            fn decode(r: &mut CdrReader<'_>) -> Result<Self, CdrError> {
                r.$m()
            }
            const MIN_SIZE: usize = $size;
        }
    )*};
}

primitive! {
    bool => bool, 1; u8 => u8, 1; i8 => i8, 1; u16 => u16, 2; i16 => i16, 2;
    u32 => u32, 4; i32 => i32, 4; u64 => u64, 8; i64 => i64, 8; f32 => f32, 4; f64 => f64, 8;
}

impl CdrEncode for String {
    fn encode(&self, w: &mut CdrWriter) {
        w.string(self)
    }
}

impl CdrDecode for String {
    fn decode(r: &mut CdrReader<'_>) -> Result<Self, CdrError> {
        r.string()
    }
    const MIN_SIZE: usize = 4;
}

impl<T: CdrEncode> CdrEncode for Vec<T> {
    fn encode(&self, w: &mut CdrWriter) {
        w.len(self.len());
        for v in self {
            v.encode(w);
        }
    }
}

impl<T: CdrDecode> CdrDecode for Vec<T> {
    fn decode(r: &mut CdrReader<'_>) -> Result<Self, CdrError> {
        let n = r.len(T::MIN_SIZE)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(T::decode(r)?);
        }
        Ok(v)
    }
    const MIN_SIZE: usize = 4;
}

impl<T: CdrEncode, const N: usize> CdrEncode for [T; N] {
    fn encode(&self, w: &mut CdrWriter) {
        for v in self {
            v.encode(w);
        }
    }
}

impl<T: CdrDecode, const N: usize> CdrDecode for [T; N] {
    fn decode(r: &mut CdrReader<'_>) -> Result<Self, CdrError> {
        let mut items = Vec::with_capacity(N);
        for _ in 0..N {
            items.push(T::decode(r)?);
        }
        Ok(items
            .try_into()
            .unwrap_or_else(|_| unreachable!("exactly N items")))
    }
    const MIN_SIZE: usize = N * T::MIN_SIZE;
}

/// Serializes a value with the encapsulation header.
pub fn to_vec<T: CdrEncode>(v: &T) -> Vec<u8> {
    let mut w = CdrWriter::new();
    v.encode(&mut w);
    w.finish()
}

/// Deserializes a value from bytes that start with the encapsulation header.
pub fn from_slice<T: CdrDecode>(b: &[u8]) -> Result<T, CdrError> {
    T::decode(&mut CdrReader::new(b)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_layout() {
        assert_eq!(
            to_vec(&"hello".to_owned()),
            [0, 1, 0, 0, 6, 0, 0, 0, b'h', b'e', b'l', b'l', b'o', 0]
        );
        assert_eq!(from_slice::<String>(&to_vec(&String::new())).unwrap(), "");
    }

    #[test]
    fn alignment_is_relative_to_the_payload() {
        let mut w = CdrWriter::new();
        w.u8(1);
        w.f64(2.0);
        w.u16(3);
        w.u32(4);
        let b = w.finish();
        // u8, 7 pad, f64, u16, 2 pad, u32
        assert_eq!(b.len(), 4 + 1 + 7 + 8 + 2 + 2 + 4);
        assert_eq!(&b[4..6], &[1, 0]);
        assert_eq!(&b[12..20], &2.0f64.to_le_bytes());
        let mut r = CdrReader::new(&b).unwrap();
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.f64().unwrap(), 2.0);
        assert_eq!(r.u16().unwrap(), 3);
        assert_eq!(r.u32().unwrap(), 4);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn sequences_and_arrays() {
        let v: Vec<f32> = vec![1.0, 2.0];
        assert_eq!(
            to_vec(&v),
            [0, 1, 0, 0, 2, 0, 0, 0, 0, 0, 0x80, 0x3f, 0, 0, 0, 0x40]
        );
        let a = [1u16, 2, 3];
        assert_eq!(to_vec(&a), [0, 1, 0, 0, 1, 0, 2, 0, 3, 0]);
        assert_eq!(from_slice::<[u16; 3]>(&to_vec(&a)).unwrap(), a);
        let nested = vec![vec!["a".to_owned()], vec![]];
        assert_eq!(
            from_slice::<Vec<Vec<String>>>(&to_vec(&nested)).unwrap(),
            nested
        );
    }

    #[test]
    fn corrupt_input_is_rejected() {
        assert_eq!(
            from_slice::<u32>(&[0, 0, 0, 0, 1, 2, 3, 4]),
            Err(CdrError::Encapsulation([0, 0, 0, 0]))
        );
        assert!(matches!(
            from_slice::<u64>(&[0, 1, 0, 0, 1, 2]),
            Err(CdrError::Truncated(_))
        ));
        // A huge length with no data behind it.
        assert!(matches!(
            from_slice::<Vec<f64>>(&[0, 1, 0, 0, 0xff, 0xff, 0xff, 0x7f]),
            Err(CdrError::BadLength(..))
        ));
        assert_eq!(
            from_slice::<String>(&[0, 1, 0, 0, 2, 0, 0, 0, b'a', b'b']),
            Err(CdrError::BadString)
        );
    }
}
