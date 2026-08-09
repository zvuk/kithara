use std::mem::size_of;

use crate::PcmSource;

const SAWTOOTH_PERIOD: u32 = 65_536;
const SAWTOOTH_CENTER: i32 = 32_768;
const I16_SCALE: f32 = 32_768.0;

/// Interleaved packed i16 input the encoder contract tests share.
pub(crate) struct TestPcm {
    bytes: Vec<u8>,
    channels: u16,
    sample_rate: u32,
}

impl TestPcm {
    #[cfg(feature = "ffmpeg")]
    pub(crate) fn from_samples(samples: &[i16], sample_rate: u32, channels: u16) -> Self {
        Self::from_bytes(
            samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect(),
            sample_rate,
            channels,
        )
    }

    #[cfg(feature = "ffmpeg")]
    pub(crate) fn from_bytes(bytes: Vec<u8>, sample_rate: u32, channels: u16) -> Self {
        Self {
            bytes,
            channels,
            sample_rate,
        }
    }

    pub(crate) fn sawtooth(frames: usize, sample_rate: u32, channels: u16) -> Self {
        let mut bytes = Vec::with_capacity(frames * usize::from(channels) * size_of::<i16>());
        for frame in 0..frames {
            let phase = u32::try_from(frame).expect("frame index fits u32") % SAWTOOTH_PERIOD;
            let centered = i32::try_from(phase).expect("sawtooth phase fits i32") - SAWTOOTH_CENTER;
            let sample = i16::try_from(centered).expect("sawtooth sample fits i16");
            for _ in 0..channels {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
        }
        Self {
            bytes,
            channels,
            sample_rate,
        }
    }

    /// The same signal as interleaved f32, scaled the way the encoder scales
    /// its own i16 input.
    pub(crate) fn samples_f32(&self) -> Vec<f32> {
        self.bytes
            .chunks_exact(size_of::<i16>())
            .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / I16_SCALE)
            .collect()
    }
}

impl PcmSource for TestPcm {
    fn channels(&self) -> u16 {
        self.channels
    }

    fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let Some(remaining) = self.bytes.get(offset..) else {
            return 0;
        };
        let read = remaining.len().min(buf.len());
        buf[..read].copy_from_slice(&remaining[..read]);
        read
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_byte_len(&self) -> Option<usize> {
        Some(self.bytes.len())
    }
}
