use kithara_blob::{Blob, BlobError, Reader, Writer};
use kithara_platform::sync::Arc;
use num_traits::ToPrimitive;

/// Wire/disk version of the stereo peak envelope.
pub const WAVEFORM_BYTES_VERSION: u32 = 2;
/// Maximum number of full-detail columns in one waveform.
pub const MAX_BUCKETS: usize = 1 << 19;

/// Why caller-supplied heights cannot be admitted.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum WaveformError {
    #[error("bucket {index} carries band height {value}, outside [0, 1]")]
    Band { index: usize, value: f32 },
    #[error("waveform carries {buckets} buckets, more than {MAX_BUCKETS}")]
    TooLarge { buckets: usize },
}

/// Four absolute companded band heights per side, in frequency order.
#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
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

/// A track's quantized stereo peak columns, indexed by normalized position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Waveform(Arc<[[u8; 8]]>);

impl Waveform {
    pub(crate) fn analysed(buckets: Vec<[u8; 8]>) -> Self {
        Self(Arc::from(buckets))
    }

    /// Decodes heights lazily without allocating another column store.
    pub fn buckets(&self) -> impl ExactSizeIterator<Item = Bucket> + DoubleEndedIterator + '_ {
        self.0.iter().copied().map(Bucket::from)
    }

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

impl TryFrom<Vec<Bucket>> for Waveform {
    type Error = WaveformError;

    fn try_from(buckets: Vec<Bucket>) -> Result<Self, WaveformError> {
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
                *byte = (255.0 * value).round().to_u8().unwrap_or(255);
            }
            quantized.push(bytes);
        }
        Ok(Self::analysed(quantized))
    }
}

impl TryFrom<&[u8]> for Waveform {
    type Error = BlobError;

    fn try_from(bytes: &[u8]) -> Result<Self, BlobError> {
        kithara_blob::from_bytes(bytes)
    }
}

impl Blob for Waveform {
    const VERSION: u32 = WAVEFORM_BYTES_VERSION;

    fn decode(r: &mut Reader<'_>) -> Result<Self, BlobError> {
        if !r.remaining().is_multiple_of(8) || r.remaining() / 8 > MAX_BUCKETS {
            return Err(BlobError::Corrupt);
        }
        let buckets = (0..r.remaining() / 8)
            .map(|_| r.read_array())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::analysed(buckets))
    }

    fn encode(&self, w: &mut Writer<'_>) {
        w.reserve(self.len() * 8);
        for bucket in self.0.iter() {
            w.write_bytes(bucket);
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_blob::to_bytes;
    use kithara_test_utils::kithara;

    use super::*;
    #[kithara::test]
    fn codec_holds_eight_bytes_and_admits_the_same_quantized_heights() {
        let input = Bucket::new([0.0, 0.25, 0.5, 1.0], [1.0, 0.5, 0.25, 0.0]);
        let wave = Waveform::try_from(vec![input]).expect("valid heights");
        let bytes = to_bytes(&wave);
        assert_eq!(bytes.len(), 4 + 8);
        assert_eq!(&bytes[4..], &[0, 64, 128, 255, 255, 128, 64, 0]);
        assert_eq!(
            Waveform::try_from(bytes.as_slice()).expect("valid blob"),
            wave
        );
        let admitted = wave.buckets().next().expect("one bucket");
        assert!((admitted.left()[1] - 0.25).abs() <= 0.5 / 255.0);
        for value in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            assert!(Waveform::try_from(vec![Bucket::new([value; 4], [0.0; 4])]).is_err());
        }
        let mut old = bytes;
        old[..4].copy_from_slice(&(WAVEFORM_BYTES_VERSION - 1).to_le_bytes());
        assert!(matches!(
            Waveform::try_from(old.as_slice()),
            Err(BlobError::Version { .. })
        ));
    }

    #[kithara::test]
    fn empty_and_oversized_waveforms_are_bounded() {
        assert!(Waveform::try_from(vec![Bucket::default(); MAX_BUCKETS + 1]).is_err());
        let empty = Waveform::try_from(Vec::new()).expect("empty valid");
        assert_eq!(
            Waveform::try_from(to_bytes(&empty).as_slice()).expect("empty blob"),
            empty
        );
        let mut bytes = to_bytes(&Waveform::try_from(vec![Bucket::default()]).expect("valid"));
        bytes.pop();
        assert!(matches!(
            Waveform::try_from(bytes.as_slice()),
            Err(BlobError::Corrupt)
        ));
    }
}
