//! PCM sample conversion helpers shared across hardware backends.
//!
//! Hardware backends (Android `MediaCodec`, Apple `AudioToolbox`) emit
//! decoded audio as interleaved little-endian `i16` or `f32` samples. These
//! helpers normalise both representations into pool-backed `f32` buffers
//! while applying a front-trim in frames (for post-seek alignment).

use std::io::Error as IoError;

use kithara_bufpool::{BudgetExhausted, PcmBuf, PcmPool};

use crate::DecodeError;

/// Decode interleaved little-endian PCM16 bytes into pool-backed `f32`.
///
/// `trim_frames` drops that many frames from the front — used to align
/// the first buffer after a `MediaCodec` seek with the requested timestamp.
pub(crate) fn decode_pcm16_to_f32(
    pool: &PcmPool,
    bytes: &[u8],
    trim_frames: usize,
    channels: u16,
) -> Result<PcmBuf, DecodeError> {
    const PCM_16_SCALE: f32 = 32_768.0;
    convert_pcm_to_f32::<2>(
        pool,
        bytes,
        trim_frames,
        channels,
        "PCM16",
        |sample_bytes| {
            let sample = i16::from_le_bytes([sample_bytes[0], sample_bytes[1]]);
            f32::from(sample) / PCM_16_SCALE
        },
    )
}

/// Copy interleaved little-endian PCM f32 bytes into a pool-backed buffer.
///
/// `trim_frames` drops that many frames from the front. When the backend
/// already produces `f32` the payload is reinterpreted byte-for-byte.
pub(crate) fn copy_pcm_float_to_pool(
    pool: &PcmPool,
    bytes: &[u8],
    trim_frames: usize,
    channels: u16,
) -> Result<PcmBuf, DecodeError> {
    convert_pcm_to_f32::<4>(
        pool,
        bytes,
        trim_frames,
        channels,
        "PCM float",
        |sample_bytes| {
            f32::from_le_bytes([
                sample_bytes[0],
                sample_bytes[1],
                sample_bytes[2],
                sample_bytes[3],
            ])
        },
    )
}

/// Convert interleaved PCM bytes into `f32` samples via `convert`, applying
/// `trim_frames` skip from the front. `label` is used only in error messages.
fn convert_pcm_to_f32<const BYTES_PER_SAMPLE: usize>(
    pool: &PcmPool,
    bytes: &[u8],
    trim_frames: usize,
    channels: u16,
    label: &'static str,
    convert: impl Fn(&[u8]) -> f32,
) -> Result<PcmBuf, DecodeError> {
    let channels = usize::from(channels);
    if channels == 0 {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "{label} output reported zero channels"
        )))));
    }
    if !bytes.len().is_multiple_of(BYTES_PER_SAMPLE) {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "{label} payload length {} is not aligned to {BYTES_PER_SAMPLE} bytes",
            bytes.len()
        )))));
    }

    let total_samples = bytes.len() / BYTES_PER_SAMPLE;
    if !total_samples.is_multiple_of(channels) {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "{label} sample count {total_samples} is not aligned to {channels} channels"
        )))));
    }

    let total_frames = total_samples / channels;
    if trim_frames >= total_frames {
        return Ok(pool.get());
    }

    let start_sample = trim_frames * channels;
    let remaining_samples = total_samples - start_sample;
    let mut pcm = pool_buffer(pool, remaining_samples)?;
    for (dst, sample_bytes) in pcm
        .iter_mut()
        .zip(bytes.chunks_exact(BYTES_PER_SAMPLE).skip(start_sample))
    {
        *dst = convert(sample_bytes);
    }

    Ok(pcm)
}

fn pool_buffer(pool: &PcmPool, samples: usize) -> Result<PcmBuf, DecodeError> {
    let mut pcm = pool.get();
    pcm.ensure_len(samples)
        .map_err(|error: BudgetExhausted| DecodeError::Backend(Box::new(error)))?;
    Ok(pcm)
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::pcm_pool;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn pcm16_conversion_normalizes_and_trims() {
        let pool = pcm_pool().clone();
        let bytes = [
            0x00_u8, 0x80, // -1.0
            0x00, 0x00, // 0.0
            0xff, 0x7f, // almost 1.0
            0x00, 0x40, // 0.5
        ];

        let pcm = decode_pcm16_to_f32(&pool, &bytes, 1, 2).expect("pcm16 conversion");

        assert_eq!(pcm.len(), 2);
        assert!((pcm[0] - (32_767.0 / 32_768.0)).abs() < 1e-6);
        assert!((pcm[1] - 0.5).abs() < 1e-6);
    }

    #[kithara::test]
    fn pcm_float_copy_is_pool_backed_and_trimmed() {
        let pool = pcm_pool().clone();
        let samples = [0.25_f32, -0.5, 0.75, -1.0];
        let bytes: Vec<u8> = samples.into_iter().flat_map(f32::to_le_bytes).collect();

        let pcm = copy_pcm_float_to_pool(&pool, &bytes, 1, 2).expect("pcm float copy");

        assert_eq!(pcm.len(), 2);
        assert!((pcm[0] - 0.75).abs() < 1e-6);
        assert!((pcm[1] + 1.0).abs() < 1e-6);
    }

    #[kithara::test]
    fn pcm16_rejects_odd_byte_length() {
        let pool = pcm_pool().clone();
        let bytes = [0u8; 3];
        let err = decode_pcm16_to_f32(&pool, &bytes, 0, 2).expect_err("should reject odd length");
        match err {
            DecodeError::Backend(_) => {}
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[kithara::test]
    fn trim_greater_than_total_returns_empty_pool_buffer() {
        let pool = pcm_pool().clone();
        let bytes = vec![0u8; 8]; // 4 i16 samples, 2 frames for stereo
        let pcm = decode_pcm16_to_f32(&pool, &bytes, 10, 2).expect("trim-over-empty");
        assert!(pcm.is_empty());
    }
}
