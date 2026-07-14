/// Cap speculative `Vec` preallocation from untrusted length prefixes.
pub(crate) const MAX_PREALLOC: usize = 4096;

/// Failure decoding an artifact byte blob. Callers treat it as a cache miss
/// and re-analyse.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlobError {
    /// Header version does not match the artifact's current encoding.
    #[error("blob version {found} != expected {expected}")]
    Version { found: u32, expected: u32 },
    /// Truncated, mis-sized, trailing garbage, or carrying an invalid value.
    #[error("blob is corrupt")]
    Corrupt,
}

/// An artifact with a versioned little-endian byte encoding. Implementors write
/// and read only the body; [`to_bytes`]/[`from_bytes`] frame the version header.
pub(crate) trait Blob: Sized {
    /// Wire/disk format version. Bump when the body encoding changes.
    const VERSION: u32;
    /// Read the body; the header version has already been validated.
    fn decode(r: &mut Reader<'_>) -> Result<Self, BlobError>;
    /// Append the body after the version header.
    fn encode(&self, w: &mut Writer<'_>);
}

/// Serialize to a versioned blob: the `u32` version, then the body.
pub(crate) fn to_bytes<T: Blob>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer(&mut out);
    w.write_u32(T::VERSION);
    value.encode(&mut w);
    out
}

/// Parse a blob produced by [`to_bytes`], rejecting a stale version or a body
/// that does not consume the blob exactly.
pub(crate) fn from_bytes<T: Blob>(bytes: &[u8]) -> Result<T, BlobError> {
    let mut r = Reader::new(bytes);
    let version = r.read_u32()?;
    if version != T::VERSION {
        return Err(BlobError::Version {
            found: version,
            expected: T::VERSION,
        });
    }
    let value = T::decode(&mut r)?;
    r.finish()?;
    Ok(value)
}

/// Little-endian append-only writer over a byte buffer.
pub(crate) struct Writer<'a>(&'a mut Vec<u8>);

impl Writer<'_> {
    pub(crate) fn reserve(&mut self, extra: usize) {
        self.0.reserve(extra);
    }

    pub(crate) fn write_f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    pub(crate) fn write_f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a length-prefixed `u64` list.
    pub(crate) fn write_frames(&mut self, frames: &[u64]) {
        self.write_len(frames.len());
        for frame in frames {
            self.write_u64(*frame);
        }
    }

    /// Write a `u64` length prefix, clamping an oversized `usize` to `u64::MAX`
    /// (a length that always fails to read back).
    pub(crate) fn write_len(&mut self, len: usize) {
        self.write_u64(u64::try_from(len).unwrap_or(u64::MAX));
    }

    pub(crate) fn write_u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    pub(crate) fn write_u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

/// Little-endian cursor reader over a byte slice.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    delegate::delegate! {
        to self.bytes {
            /// Succeed only if the whole blob was consumed.
            #[expr(if self.cursor == $ {
                        Ok(())
                    } else {
                        Err(BlobError::Corrupt)
                    })]
            #[call(len)]
            pub (crate) fn finish (& self) -> Result < () , BlobError >;
            /// Bytes not yet consumed.
            #[expr($ - self.cursor)]
            #[call(len)]
            pub (crate) fn remaining (& self) -> usize;
        }
    }
    pub(crate) fn read_array<const N: usize>(&mut self) -> Result<[u8; N], BlobError> {
        let end = self.cursor.checked_add(N).ok_or(BlobError::Corrupt)?;
        let chunk = self.bytes.get(self.cursor..end).ok_or(BlobError::Corrupt)?;
        let mut out = [0u8; N];
        out.copy_from_slice(chunk);
        self.cursor = end;
        Ok(out)
    }

    pub(crate) fn read_f32(&mut self) -> Result<f32, BlobError> {
        Ok(f32::from_le_bytes(self.read_array::<4>()?))
    }

    pub(crate) fn read_f64(&mut self) -> Result<f64, BlobError> {
        Ok(f64::from_le_bytes(self.read_array::<8>()?))
    }

    /// Read a length-prefixed `u64` list, capping preallocation.
    pub(crate) fn read_frames(&mut self) -> Result<Vec<u64>, BlobError> {
        let count = self.read_len()?;
        let mut out = Vec::with_capacity(count.min(MAX_PREALLOC));
        for _ in 0..count {
            out.push(self.read_u64()?);
        }
        Ok(out)
    }

    /// Read a `u64` length prefix as a `usize`.
    pub(crate) fn read_len(&mut self) -> Result<usize, BlobError> {
        usize::try_from(self.read_u64()?).map_err(|_| BlobError::Corrupt)
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32, BlobError> {
        Ok(u32::from_le_bytes(self.read_array::<4>()?))
    }

    pub(crate) fn read_u64(&mut self) -> Result<u64, BlobError> {
        Ok(u64::from_le_bytes(self.read_array::<8>()?))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Blob, BlobError, Reader, Writer, from_bytes, to_bytes};

    /// Minimal artifact exercising the header framing in isolation.
    #[derive(Debug, PartialEq)]
    struct Pair {
        b: f64,
        a: u64,
    }

    impl Blob for Pair {
        const VERSION: u32 = 7;

        fn decode(r: &mut Reader<'_>) -> Result<Self, BlobError> {
            Ok(Self {
                a: r.read_u64()?,
                b: r.read_f64()?,
            })
        }

        fn encode(&self, w: &mut Writer<'_>) {
            w.write_u64(self.a);
            w.write_f64(self.b);
        }
    }

    #[kithara::test]
    fn round_trips_with_version_header() {
        let pair = Pair { a: 42, b: 1.5 };
        let bytes = to_bytes(&pair);
        assert_eq!(bytes[..4], 7u32.to_le_bytes());
        assert_eq!(from_bytes::<Pair>(&bytes).expect("round-trips"), pair);
    }

    #[kithara::test]
    fn rejects_wrong_version() {
        let mut bytes = to_bytes(&Pair { a: 1, b: 2.0 });
        bytes[0] = bytes[0].wrapping_add(1);
        assert!(matches!(
            from_bytes::<Pair>(&bytes),
            Err(BlobError::Version { expected: 7, .. })
        ));
    }

    #[kithara::test]
    fn rejects_short_and_trailing() {
        assert!(matches!(
            from_bytes::<Pair>(&[0, 0]),
            Err(BlobError::Corrupt)
        ));

        let mut trailing = to_bytes(&Pair { a: 1, b: 2.0 });
        trailing.push(0);
        assert!(matches!(
            from_bytes::<Pair>(&trailing),
            Err(BlobError::Corrupt)
        ));
    }
}
