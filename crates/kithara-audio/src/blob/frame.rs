use super::{Reader, Writer};

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
    let mut writer = Writer::new(&mut out);
    writer.write_u32(T::VERSION);
    value.encode(&mut writer);
    out
}

/// Parse a blob produced by [`to_bytes`], rejecting a stale version or a body
/// that does not consume the blob exactly.
pub(crate) fn from_bytes<T: Blob>(bytes: &[u8]) -> Result<T, BlobError> {
    let mut reader = Reader::new(bytes);
    let version = reader.read_u32()?;
    if version != T::VERSION {
        return Err(BlobError::Version {
            found: version,
            expected: T::VERSION,
        });
    }
    let value = T::decode(&mut reader)?;
    reader.finish()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[derive(Debug, PartialEq)]
    struct Pair {
        b: f64,
        a: u64,
    }

    impl Blob for Pair {
        const VERSION: u32 = 7;

        fn decode(reader: &mut Reader<'_>) -> Result<Self, BlobError> {
            Ok(Self {
                a: reader.read_u64()?,
                b: reader.read_f64()?,
            })
        }

        fn encode(&self, writer: &mut Writer<'_>) {
            writer.write_u64(self.a);
            writer.write_f64(self.b);
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
