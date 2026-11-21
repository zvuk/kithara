use std::{mem::size_of, num::NonZeroU64};

use kithara_blob::{Blob, BlobError, Reader, Writer};
use kithara_platform::sync::Arc;
use num_traits::ToPrimitive;

/// Wire/disk format version for the [`Waveform`] blob. Bump when the encoding
/// or the analysis parameters change.
pub const WAVEFORM_BYTES_VERSION: u32 = 3;

/// Most buckets a waveform may carry. The analyzer coarsens its fold factor
/// when native columns would exceed this format limit.
pub const MAX_BUCKETS: usize = 1 << 19;

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

/// Four absolute companded band heights per side, in frequency order.
#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct Bucket {
    #[field(get, copy)]
    left: [f32; 4],
    #[field(get, copy)]
    right: [f32; 4],
}

impl Bucket {
    #[must_use]
    pub const fn new(left: [f32; 4], right: [f32; 4]) -> Self {
        Self { left, right }
    }
}

impl From<[u8; 8]> for Bucket {
    fn from(bytes: [u8; 8]) -> Self {
        let [l0, l1, l2, l3, r0, r1, r2, r3] = bytes.map(|b| f32::from(b) / 255.0);
        Self::new([l0, l1, l2, l3], [r0, r1, r2, r3])
    }
}

/// A track's analysed waveform: per-bucket band heights in `[0, 1]`.
/// Bucket `b` starts at source frame `b * span_frames`.
#[derive(Clone, Debug, PartialEq, Eq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct Waveform {
    buckets: Arc<[[u8; 8]]>,
    /// Source frames between consecutive bucket starts.
    #[field(get, copy)]
    span_frames: NonZeroU64,
}

impl Waveform {
    pub(crate) fn analysed(buckets: Vec<[u8; 8]>, span_frames: NonZeroU64) -> Self {
        Self {
            buckets: Arc::from(buckets),
            span_frames,
        }
    }

    /// Decodes heights lazily without allocating another column store.
    pub fn buckets(&self) -> impl ExactSizeIterator<Item = Bucket> + DoubleEndedIterator + '_ {
        self.buckets.iter().copied().map(Bucket::from)
    }

    /// Append the versioned waveform encoding to caller-owned storage.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        kithara_blob::write_to(self, out);
    }

    /// The checked way in for a waveform a caller already holds: one served by
    /// a backend, or one restored from a store outside this crate. It admits
    /// exactly what the byte codec admits, so a structure and a blob cannot
    /// disagree about what a valid waveform is.
    ///
    /// # Errors
    /// Returns [`WaveformError`] for too many buckets or a band outside `[0, 1]`.
    pub fn try_new(buckets: Vec<Bucket>, span_frames: NonZeroU64) -> Result<Self, WaveformError> {
        if buckets.len() > MAX_BUCKETS {
            return Err(WaveformError::TooLarge {
                buckets: buckets.len(),
            });
        }
        let mut quantized: Vec<[u8; 8]> = Vec::with_capacity(buckets.len());
        for (index, bucket) in buckets.into_iter().enumerate() {
            let mut bytes = [0; 8];
            for (byte, value) in bytes
                .iter_mut()
                .zip(bucket.left.into_iter().chain(bucket.right))
            {
                if !(0.0..=1.0).contains(&value) {
                    return Err(WaveformError::Band { index, value });
                }
                *byte = quantize(value);
            }
            quantized.push(bytes);
        }
        Ok(Self::analysed(quantized, span_frames))
    }

    delegate::delegate! {
        to self.buckets {
            #[must_use]
            pub fn is_empty(&self) -> bool;
            #[must_use]
            pub fn len(&self) -> usize;
        }
    }
}

/// Parses the versioned bytes written by [`Waveform::write_to`]. A version mismatch, a
/// zero span, a body that is not a whole number of buckets, or too many columns
/// is a typed error the caller treats as a cache miss.
///
/// Returns [`BlobError::Version`] when the header version does not match
/// [`WAVEFORM_BYTES_VERSION`], and [`BlobError::Corrupt`] when the span is zero
/// or the body is truncated, mis-sized, or exceeds the column limit.
impl TryFrom<&[u8]> for Waveform {
    type Error = BlobError;

    fn try_from(bytes: &[u8]) -> Result<Self, BlobError> {
        kithara_blob::from_bytes(bytes)
    }
}

impl Blob for Waveform {
    const VERSION: u32 = WAVEFORM_BYTES_VERSION;

    fn decode(r: &mut Reader<'_>) -> Result<Self, BlobError> {
        let span_frames = NonZeroU64::new(r.read_u64()?).ok_or(BlobError::Corrupt)?;
        if !r.remaining().is_multiple_of(8) || r.remaining() / 8 > MAX_BUCKETS {
            return Err(BlobError::Corrupt);
        }
        let buckets = (0..r.remaining() / 8)
            .map(|_| r.read_array())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::analysed(buckets, span_frames))
    }

    fn encode(&self, w: &mut Writer<'_>) {
        w.reserve(size_of::<u64>() + self.len() * 8);
        w.write_u64(self.span_frames.get());
        for bucket in self.buckets.iter() {
            w.write_bytes(bucket);
        }
    }
}

pub(crate) fn quantize(height: f32) -> u8 {
    (255.0 * height).round().to_u8().unwrap_or(255)
}

#[cfg(test)]
mod value_tests {
    use kithara_test_utils::kithara;

    use super::{Bucket, MAX_BUCKETS, NonZeroU64, Waveform, WaveformError};

    #[kithara::test]
    #[case(f32::NAN)]
    #[case(f32::INFINITY)]
    #[case(-0.1)]
    #[case(1.1)]
    fn a_band_outside_the_painted_range_is_refused(#[case] height: f32) {
        let buckets = vec![
            Bucket::new([0.5; 4], [0.5; 4]),
            Bucket::new([0.0, height, 0.0, 0.0], [0.0; 4]),
        ];
        let refused =
            Waveform::try_new(buckets, NonZeroU64::MIN).expect_err("the band is out of range");
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
            Waveform::try_new(buckets, NonZeroU64::MIN).expect_err("the waveform is too long"),
            WaveformError::TooLarge {
                buckets: MAX_BUCKETS + 1
            }
        );
    }

    #[kithara::test]
    fn buckets_inside_the_range_are_kept_in_order() {
        let buckets = vec![
            Bucket::from([0, 128, 192, 255, 1, 64, 100, 254]),
            Bucket::from([255, 0, 64, 128, 200, 100, 50, 25]),
        ];
        let wave = Waveform::try_new(buckets.clone(), NonZeroU64::MIN)
            .expect("every band is inside [0, 1]");
        assert!(wave.buckets().eq(buckets));
    }
}

#[cfg(test)]
mod bytes_tests {
    use kithara_blob::{BlobError, to_bytes};
    use kithara_test_utils::kithara;

    use super::{Bucket, NonZeroU64, WAVEFORM_BYTES_VERSION, Waveform};

    fn sample() -> Waveform {
        Waveform::try_new(
            vec![
                Bucket::new([0.1, 0.2, 0.3, 0.4], [0.8, 0.7, 0.6, 0.5]),
                Bucket::new([0.0, 1.0, 0.5, 0.25], [0.25, 0.5, 1.0, 0.0]),
            ],
            NonZeroU64::new(128).unwrap(),
        )
        .expect("hand-built buckets are in range")
    }

    #[kithara::test]
    fn round_trips() {
        let wave = sample();
        let bytes = to_bytes(&wave);
        let back = Waveform::try_from(bytes.as_slice()).expect("valid blob round-trips");
        assert_eq!(back, wave);
        assert_eq!(back.span_frames(), wave.span_frames());
        let another_span = Waveform::try_new(wave.buckets().collect(), NonZeroU64::MIN)
            .expect("the same heights are valid");
        assert_ne!(another_span, wave, "the span is part of waveform equality");
    }

    #[kithara::test]
    fn a_span_larger_than_u32_round_trips_as_u64() {
        let span = NonZeroU64::new(u64::from(u32::MAX) * 64).unwrap();
        let wave = Waveform::try_new(vec![Bucket::new([0.5; 4], [1.0; 4])], span).unwrap();
        let bytes = to_bytes(&wave);
        assert_eq!(&bytes[4..12], &span.get().to_le_bytes());
        let back = Waveform::try_from(bytes.as_slice()).unwrap();
        assert_eq!(back, wave);
    }

    #[kithara::test]
    fn empty_round_trips() {
        let wave =
            Waveform::try_new(Vec::new(), NonZeroU64::MIN).expect("no buckets is a valid waveform");
        let bytes = to_bytes(&wave);
        let back = Waveform::try_from(bytes.as_slice()).expect("empty blob round-trips");
        assert!(back.is_empty());
    }

    #[kithara::test]
    fn heights_are_quantized_to_eight_bytes() {
        let input = Bucket::new([0.0, 0.25, 0.5, 1.0], [1.0, 0.5, 0.25, 0.0]);
        let wave = Waveform::try_new(vec![input], NonZeroU64::MIN).expect("valid heights");
        let bytes = to_bytes(&wave);
        assert_eq!(bytes.len(), 20);
        assert_eq!(&bytes[12..], &[0, 64, 128, 255, 255, 128, 64, 0]);
        let admitted = wave.buckets().next().expect("one bucket");
        assert!((admitted.left()[1] - 0.25).abs() <= 0.5 / 255.0);
    }

    /// Version 1 is the FFT format and version 2 the fold factor's three-band
    /// format; both are a typed version error, not a misread.
    #[kithara::test]
    #[case(1)]
    #[case(2)]
    #[case(WAVEFORM_BYTES_VERSION + 1)]
    fn rejects_wrong_version(#[case] version: u32) {
        let mut bytes = to_bytes(&sample());
        bytes[..4].copy_from_slice(&version.to_le_bytes());
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

        let mut zero_span = to_bytes(&sample());
        zero_span[4..12].fill(0);
        assert!(corrupt(zero_span), "a bucket span must be nonzero");

        let mut truncated = to_bytes(&sample());
        truncated.pop();
        assert!(corrupt(truncated), "body not a whole number of buckets");

        let mut oversized = vec![0; 12 + 8 * (super::MAX_BUCKETS + 1)];
        oversized[..4].copy_from_slice(&WAVEFORM_BYTES_VERSION.to_le_bytes());
        oversized[4..12].copy_from_slice(&64_u64.to_le_bytes());
        assert!(corrupt(oversized), "body exceeds the column limit");
    }
}
