use std::sync::Arc;

/// Wire/disk format version for [`Waveform::to_bytes`]. Bump when the encoding,
/// the analysis parameters, or the caller's bucket resolution changes.
pub(crate) const WAVEFORM_BYTES_VERSION: u32 = 1;

/// Bytes per serialized bucket: three little-endian `f32` band heights.
const BUCKET_BYTES: usize = 12;

/// Failure decoding a [`Waveform`] byte blob. Callers treat it as a cache miss
/// and re-analyse.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WaveformBytesError {
    /// Header version does not match [`WAVEFORM_BYTES_VERSION`] (stale cache).
    #[error("waveform blob version {found} != expected {expected}")]
    Version { found: u32, expected: u32 },
    /// Truncated, mis-sized, or carrying a band height outside `[0, 1]`.
    #[error("waveform blob is corrupt")]
    Corrupt,
}

/// One waveform column: three normalized frequency-band heights, each in
/// `[0, 1]` on a shared scale after per-band perceptual gain. The deck paints
/// them as concentric mirrored bars (low behind, high in front), so all three
/// bands are visible at once. All-zero is silence and renders as nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Bucket {
    pub low: f32,
    pub mid: f32,
    pub high: f32,
}

/// A track's analysed waveform: per-bucket band heights in `[0, 1]`, indexed by
/// normalized track position. Built by [`WaveformAnalyzer::finalize`] or
/// [`Waveform::from_bytes`]; both uphold the `[0, 1]` invariant.
///
/// [`WaveformAnalyzer::finalize`]: super::WaveformAnalyzer::finalize
#[derive(Clone, Debug)]
pub struct Waveform(Arc<[Bucket]>);

impl Waveform {
    pub(crate) fn from_buckets(buckets: Vec<Bucket>) -> Self {
        Self(Arc::from(buckets))
    }

    #[must_use]
    pub fn buckets(&self) -> &[Bucket] {
        &self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Serialize to a versioned little-endian blob: a `u32` version, then three
    /// `f32` band heights per bucket.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.0.len() * BUCKET_BYTES);
        out.extend_from_slice(&WAVEFORM_BYTES_VERSION.to_le_bytes());
        for b in self.0.iter() {
            out.extend_from_slice(&b.low.to_le_bytes());
            out.extend_from_slice(&b.mid.to_le_bytes());
            out.extend_from_slice(&b.high.to_le_bytes());
        }
        out
    }

    /// Parse a blob produced by [`Self::to_bytes`]. A version mismatch, a body
    /// that is not a whole number of buckets, or a band height outside `[0, 1]`
    /// is a typed error the caller treats as a cache miss.
    ///
    /// # Errors
    ///
    /// Returns [`WaveformBytesError::Version`] when the header version does not
    /// match [`WAVEFORM_BYTES_VERSION`], and [`WaveformBytesError::Corrupt`]
    /// when the header is missing.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WaveformBytesError> {
        let header = bytes.get(0..4).ok_or(WaveformBytesError::Corrupt)?;
        let version = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        if version != WAVEFORM_BYTES_VERSION {
            return Err(WaveformBytesError::Version {
                found: version,
                expected: WAVEFORM_BYTES_VERSION,
            });
        }
        let body = &bytes[4..];
        if !body.len().is_multiple_of(BUCKET_BYTES) {
            return Err(WaveformBytesError::Corrupt);
        }
        
        let mut buckets = Vec::with_capacity(body.len() / BUCKET_BYTES);
        for c in body.chunks_exact(BUCKET_BYTES) {
            let low = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            let mid = f32::from_le_bytes([c[4], c[5], c[6], c[7]]);
            let high = f32::from_le_bytes([c[8], c[9], c[10], c[11]]);
            // `(0.0..=1.0).contains` is false for NaN/infinities, so this one
            // check covers finiteness and the `[0, 1]` invariant.
            let ok = |v: f32| (0.0..=1.0).contains(&v);
            if !(ok(low) && ok(mid) && ok(high)) {
                return Err(WaveformBytesError::Corrupt);
            }
            buckets.push(Bucket { low, mid, high });
        }
        Ok(Self::from_buckets(buckets))
    }
}

#[cfg(test)]
mod bytes_tests {
    use kithara_test_utils::kithara;

    use super::{Bucket, WAVEFORM_BYTES_VERSION, Waveform, WaveformBytesError};

    fn sample() -> Waveform {
        Waveform::from_buckets(vec![
            Bucket {
                low: 0.1,
                mid: 0.2,
                high: 0.3,
            },
            Bucket {
                low: 0.0,
                mid: 1.0,
                high: 0.5,
            },
        ])
    }

    #[kithara::test]
    fn round_trips() {
        let wave = sample();
        let bytes = wave.to_bytes();
        let back = Waveform::from_bytes(&bytes).expect("valid blob round-trips");
        assert_eq!(back.buckets(), wave.buckets());
    }

    #[kithara::test]
    fn empty_round_trips() {
        let wave = Waveform::from_buckets(Vec::new());
        let back = Waveform::from_bytes(&wave.to_bytes()).expect("empty blob round-trips");
        assert!(back.is_empty());
    }

    #[kithara::test]
    fn rejects_wrong_version() {
        let mut bytes = sample().to_bytes();
        bytes[0] = bytes[0].wrapping_add(1);
        assert!(matches!(
            Waveform::from_bytes(&bytes),
            Err(WaveformBytesError::Version { expected, .. }) if expected == WAVEFORM_BYTES_VERSION
        ));
    }

    #[kithara::test]
    fn rejects_corrupt_blobs() {
        let corrupt = |bytes: Vec<u8>| {
            matches!(
                Waveform::from_bytes(&bytes),
                Err(WaveformBytesError::Corrupt)
            )
        };

        assert!(corrupt(vec![0, 0]), "shorter than the version header");

        let mut truncated = sample().to_bytes();
        truncated.pop();
        assert!(corrupt(truncated), "body not a whole number of buckets");

        let nan_at_end = |v: f32| {
            let mut bytes = sample().to_bytes();
            let tail = bytes.len() - 4;
            bytes[tail..].copy_from_slice(&v.to_le_bytes());
            bytes
        };
        // NaN and a finite out-of-[0,1] height both survive the renderer's
        // clamp silently, so both must be rejected to keep the invariant.
        assert!(corrupt(nan_at_end(f32::NAN)), "non-finite band height");
        assert!(corrupt(nan_at_end(5.0)), "finite out-of-range band height");
    }
}
