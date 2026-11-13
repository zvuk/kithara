use std::mem::size_of;

use kithara_blob::{Blob, BlobError, MAX_PREALLOC, Reader, Writer};
use kithara_platform::sync::Arc;

use crate::Band;

/// Wire/disk format version for the [`Waveform`] blob. Bump when the encoding,
/// the analysis parameters, or the caller's bucket resolution changes.
pub const WAVEFORM_BYTES_VERSION: u32 = 1;

/// Most buckets a waveform may carry. A waveform is a display column per
/// bucket, so a track needs thousands; anything past this is a caller or a
/// stored artifact that disagrees with this format, not a longer track.
pub const MAX_BUCKETS: usize = 1 << 16;

/// Why a caller-supplied waveform is not one this crate will hold.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum WaveformError {
    /// A band height is not finite, or lies outside the normalized `[0, 1]`
    /// range every consumer paints against.
    #[error("bucket {index} carries band height {value}, which is not inside [0, 1]")]
    Band {
        /// Position of the offending bucket.
        index: usize,
        /// The height as supplied.
        value: f32,
    },
    /// More buckets than [`MAX_BUCKETS`].
    #[error("waveform carries {buckets} buckets, more than the {MAX_BUCKETS} this format holds")]
    TooLarge {
        /// The bucket count as supplied.
        buckets: usize,
    },
}

/// One waveform column: three normalized frequency-band heights, each in
/// `[0, 1]` on a shared scale after per-band perceptual gain. The deck paints
/// them as concentric mirrored bars (low behind, high in front), so all three
/// bands are visible at once. All-zero is silence and renders as nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct Bucket {
    high: f32,
    low: f32,
    mid: f32,
}

impl Bucket {
    #[must_use]
    pub const fn new(low: f32, mid: f32, high: f32) -> Self {
        Self { high, low, mid }
    }

    /// Height of one band — the order the analyzer and the wire both use.
    pub(crate) const fn band(self, band: Band) -> f32 {
        match band {
            Band::Low => self.low,
            Band::Mid => self.mid,
            Band::High => self.high,
        }
    }
}

/// A track's analysed waveform: per-bucket band heights in `[0, 1]`, indexed by
/// normalized track position.
#[derive(Clone, Debug, Default)]
pub struct Waveform(Arc<[Bucket]>);

impl Waveform {
    /// Bytes per serialized bucket: three little-endian `f32` band heights.
    const BUCKET_BYTES: usize = Band::COUNT * size_of::<f32>();

    /// Take buckets this crate's own analyzer produced: it normalizes every
    /// band into `[0, 1]` before it fills one, so the check a caller's
    /// waveform goes through has nothing left to reject here.
    pub(crate) fn analysed(buckets: Vec<Bucket>) -> Self {
        Self(Arc::from(buckets))
    }

    #[must_use]
    pub fn buckets(&self) -> &[Bucket] {
        &self.0
    }

    /// Append the versioned waveform encoding to caller-owned storage.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        kithara_blob::write_to(self, out);
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

/// The checked way in for a waveform a caller already holds — one served by a
/// backend, or one restored from a store outside this crate. It admits exactly
/// what the byte codec admits, so a structure and a blob cannot disagree about
/// what a valid waveform is.
impl TryFrom<Vec<Bucket>> for Waveform {
    type Error = WaveformError;

    fn try_from(buckets: Vec<Bucket>) -> Result<Self, WaveformError> {
        if buckets.len() > MAX_BUCKETS {
            return Err(WaveformError::TooLarge {
                buckets: buckets.len(),
            });
        }
        for (index, bucket) in buckets.iter().enumerate() {
            for band in Band::ALL {
                let value = bucket.band(band);
                if !(0.0..=1.0).contains(&value) {
                    return Err(WaveformError::Band { index, value });
                }
            }
        }
        Ok(Self::analysed(buckets))
    }
}

/// Parses the versioned bytes written by [`Waveform::write_to`]. A version mismatch, a
/// body that is not a whole number of buckets, or a band height outside `[0, 1]`
/// is a typed error the caller treats as a cache miss.
///
/// Returns [`BlobError::Version`] when the header version does not match
/// [`WAVEFORM_BYTES_VERSION`], and [`BlobError::Corrupt`] when the body is
/// truncated, mis-sized, or carries a band height outside `[0, 1]`.
impl TryFrom<&[u8]> for Waveform {
    type Error = BlobError;

    fn try_from(bytes: &[u8]) -> Result<Self, BlobError> {
        kithara_blob::from_bytes(bytes)
    }
}

impl Blob for Waveform {
    const VERSION: u32 = WAVEFORM_BYTES_VERSION;

    fn decode(r: &mut Reader<'_>) -> Result<Self, BlobError> {
        if !r.remaining().is_multiple_of(Self::BUCKET_BYTES) {
            return Err(BlobError::Corrupt);
        }
        let count = r.remaining() / Self::BUCKET_BYTES;
        let mut buckets: Vec<Bucket> = Vec::with_capacity(count.min(MAX_PREALLOC));
        for _ in 0..count {
            let mut heights = [0.0; Band::COUNT];
            for height in &mut heights {
                *height = r.read_f32()?;
            }
            buckets.push(Bucket::new(
                heights[Band::Low.idx()],
                heights[Band::Mid.idx()],
                heights[Band::High.idx()],
            ));
        }
        Self::try_from(buckets).map_err(|_| BlobError::Corrupt)
    }

    fn encode(&self, w: &mut Writer<'_>) {
        w.reserve(self.0.len() * Self::BUCKET_BYTES);
        for b in self.0.iter() {
            for band in Band::ALL {
                w.write_f32(b.band(band));
            }
        }
    }
}

#[cfg(test)]
mod value_tests {
    use kithara_test_utils::kithara;

    use super::{Bucket, MAX_BUCKETS, Waveform, WaveformError};

    #[kithara::test]
    #[case(f32::NAN)]
    #[case(f32::INFINITY)]
    #[case(-0.1)]
    #[case(1.1)]
    fn a_band_outside_the_painted_range_is_refused(#[case] height: f32) {
        let buckets = vec![Bucket::new(0.5, 0.5, 0.5), Bucket::new(0.0, height, 0.0)];
        let refused = Waveform::try_from(buckets).expect_err("the band is out of range");
        match refused {
            WaveformError::Band { index, value } => {
                assert_eq!(index, 1);
                assert_eq!(value.is_nan(), height.is_nan());
                assert!(value.is_nan() || value == height);
            }
            other => panic!("expected a band error, got {other}"),
        }
    }

    #[kithara::test]
    fn more_buckets_than_the_format_holds_are_refused() {
        let buckets = vec![Bucket::default(); MAX_BUCKETS + 1];
        assert_eq!(
            Waveform::try_from(buckets).expect_err("the waveform is too long"),
            WaveformError::TooLarge {
                buckets: MAX_BUCKETS + 1
            }
        );
    }

    #[kithara::test]
    fn buckets_inside_the_range_are_kept_in_order() {
        let buckets = vec![Bucket::new(0.0, 0.5, 1.0), Bucket::new(1.0, 0.0, 0.25)];
        let wave = Waveform::try_from(buckets.clone()).expect("every band is inside [0, 1]");
        assert_eq!(wave.buckets(), buckets.as_slice());
    }
}

#[cfg(test)]
mod bytes_tests {
    use kithara_blob::{BlobError, to_bytes};
    use kithara_test_utils::kithara;

    use super::{Bucket, WAVEFORM_BYTES_VERSION, Waveform};

    fn sample() -> Waveform {
        Waveform::try_from(vec![Bucket::new(0.1, 0.2, 0.3), Bucket::new(0.0, 1.0, 0.5)])
            .expect("hand-built buckets are in range")
    }

    #[kithara::test]
    fn round_trips() {
        let wave = sample();
        let bytes = to_bytes(&wave);
        let back = Waveform::try_from(bytes.as_slice()).expect("valid blob round-trips");
        assert_eq!(back.buckets(), wave.buckets());
    }

    #[kithara::test]
    fn empty_round_trips() {
        let wave = Waveform::try_from(Vec::new()).expect("no buckets is a valid waveform");
        let bytes = to_bytes(&wave);
        let back = Waveform::try_from(bytes.as_slice()).expect("empty blob round-trips");
        assert!(back.is_empty());
    }

    #[kithara::test]
    fn rejects_wrong_version() {
        let mut bytes = to_bytes(&sample());
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

        let mut truncated = to_bytes(&sample());
        truncated.pop();
        assert!(corrupt(truncated), "body not a whole number of buckets");

        let nan_at_end = |v: f32| {
            let mut bytes = to_bytes(&sample());
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
