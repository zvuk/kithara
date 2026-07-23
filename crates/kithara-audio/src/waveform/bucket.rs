use kithara_platform::sync::Arc;

use crate::blob::{self, Blob, BlobError, Reader, Writer};

/// Wire/disk format version for the [`Waveform`] blob. Bump when the encoding,
/// the analysis parameters, or the caller's bucket resolution changes.
pub(crate) const WAVEFORM_BYTES_VERSION: u32 = 1;

/// Bytes per serialized bucket: three little-endian `f32` band heights.
const BUCKET_BYTES: usize = 12;

/// One waveform column: three normalized frequency-band heights, each in
/// `[0, 1]` on a shared scale after per-band perceptual gain. The deck paints
/// them as concentric mirrored bars (low behind, high in front), so all three
/// bands are visible at once. All-zero is silence and renders as nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Bucket {
    high: f32,
    low: f32,
    mid: f32,
}

impl Bucket {
    #[must_use]
    pub fn new(low: f32, mid: f32, high: f32) -> Self {
        Self { high, low, mid }
    }

    #[must_use]
    pub fn high(&self) -> f32 {
        self.high
    }

    #[must_use]
    pub fn low(&self) -> f32 {
        self.low
    }

    #[must_use]
    pub fn mid(&self) -> f32 {
        self.mid
    }
}

/// A track's analysed waveform: per-bucket band heights in `[0, 1]`, indexed by
/// normalized track position.
#[derive(Clone, Debug)]
pub struct Waveform(Arc<[Bucket]>);

impl Waveform {
    #[must_use]
    pub fn buckets(&self) -> &[Bucket] {
        &self.0
    }

    delegate::delegate! {
        to self.0 {
            #[must_use]
            pub fn is_empty(&self) -> bool;
            #[must_use]
            pub fn len(&self) -> usize;
        }
    }
}

impl From<Vec<Bucket>> for Waveform {
    fn from(buckets: Vec<Bucket>) -> Self {
        Self(Arc::from(buckets))
    }
}

/// Serialize to a versioned little-endian blob: a `u32` version, then three
/// `f32` band heights per bucket.
impl From<&Waveform> for Vec<u8> {
    fn from(wave: &Waveform) -> Self {
        blob::to_bytes(wave)
    }
}

/// Parse a blob produced by `Vec::<u8>::from(&Waveform)`. A version mismatch, a
/// body that is not a whole number of buckets, or a band height outside `[0, 1]`
/// is a typed error the caller treats as a cache miss.
///
/// Returns [`BlobError::Version`] when the header version does not match
/// [`WAVEFORM_BYTES_VERSION`], and [`BlobError::Corrupt`] when the body is
/// truncated, mis-sized, or carries a band height outside `[0, 1]`.
impl TryFrom<&[u8]> for Waveform {
    type Error = BlobError;

    fn try_from(bytes: &[u8]) -> Result<Self, BlobError> {
        blob::from_bytes(bytes)
    }
}

impl Blob for Waveform {
    const VERSION: u32 = WAVEFORM_BYTES_VERSION;

    fn decode(r: &mut Reader<'_>) -> Result<Self, BlobError> {
        if !r.remaining().is_multiple_of(BUCKET_BYTES) {
            return Err(BlobError::Corrupt);
        }
        let count = r.remaining() / BUCKET_BYTES;
        let mut buckets = Vec::with_capacity(count);
        for _ in 0..count {
            let low = r.read_f32()?;
            let mid = r.read_f32()?;
            let high = r.read_f32()?;
            // `(0.0..=1.0).contains` is false for NaN/infinities, so this one
            // check covers finiteness and the `[0, 1]` invariant.
            let ok = |v: f32| (0.0..=1.0).contains(&v);
            if !(ok(low) && ok(mid) && ok(high)) {
                return Err(BlobError::Corrupt);
            }
            buckets.push(Bucket::new(low, mid, high));
        }
        Ok(Self::from(buckets))
    }

    fn encode(&self, w: &mut Writer<'_>) {
        w.reserve(self.0.len() * BUCKET_BYTES);
        for b in self.0.iter() {
            w.write_f32(b.low());
            w.write_f32(b.mid());
            w.write_f32(b.high());
        }
    }
}

#[cfg(test)]
mod bytes_tests {
    use kithara_test_utils::kithara;

    use super::{Bucket, WAVEFORM_BYTES_VERSION, Waveform};
    use crate::blob::BlobError;

    fn sample() -> Waveform {
        Waveform::from(vec![Bucket::new(0.1, 0.2, 0.3), Bucket::new(0.0, 1.0, 0.5)])
    }

    #[kithara::test]
    fn round_trips() {
        let wave = sample();
        let bytes = Vec::<u8>::from(&wave);
        let back = Waveform::try_from(bytes.as_slice()).expect("valid blob round-trips");
        assert_eq!(back.buckets(), wave.buckets());
    }

    #[kithara::test]
    fn empty_round_trips() {
        let wave = Waveform::from(Vec::new());
        let bytes = Vec::<u8>::from(&wave);
        let back = Waveform::try_from(bytes.as_slice()).expect("empty blob round-trips");
        assert!(back.is_empty());
    }

    #[kithara::test]
    fn rejects_wrong_version() {
        let mut bytes = Vec::<u8>::from(&sample());
        bytes[0] = bytes[0].wrapping_add(1);
        assert!(matches!(
            Waveform::try_from(bytes.as_slice()),
            Err(BlobError::Version { expected, .. }) if expected == WAVEFORM_BYTES_VERSION
        ));
    }

    #[kithara::test]
    fn rejects_corrupt_blobs() {
        let corrupt = |bytes: Vec<u8>| {
            matches!(
                Waveform::try_from(bytes.as_slice()),
                Err(BlobError::Corrupt)
            )
        };

        assert!(corrupt(vec![0, 0]), "shorter than the version header");

        let mut truncated = Vec::<u8>::from(&sample());
        truncated.pop();
        assert!(corrupt(truncated), "body not a whole number of buckets");

        let nan_at_end = |v: f32| {
            let mut bytes = Vec::<u8>::from(&sample());
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
