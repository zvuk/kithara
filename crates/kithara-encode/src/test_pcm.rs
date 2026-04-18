//! PCM test fixtures shared by encoder unit tests.

use std::mem::size_of;

use crate::PcmSource;

/// Interleaved PCM buffer: each frame repeats one 16-bit little-endian sawtooth sample per channel.
pub(crate) struct SawtoothPcmFixture {
    sample_rate: u32,
    channels: u16,
    bytes: Vec<u8>,
}

impl SawtoothPcmFixture {
    const SAWTOOTH_PERIOD: usize = 65_536;
    const SAWTOOTH_CENTER: i32 = 32_768;

    pub(crate) fn new(total_frames: usize, sample_rate: u32, channels: u16) -> Self {
        let mut bytes = Vec::with_capacity(
            total_frames.saturating_mul(usize::from(channels)) * size_of::<i16>(),
        );
        for frame in 0..total_frames {
            let sample = Self::sawtooth_sample_i16(frame);
            let sample_bytes = sample.to_le_bytes();
            for _ in 0..channels {
                bytes.extend_from_slice(&sample_bytes);
            }
        }
        Self {
            sample_rate,
            channels,
            bytes,
        }
    }

    /// One sample of a 16-bit little-endian sawtooth, repeating every [`Self::SAWTOOTH_PERIOD`] frames.
    fn sawtooth_sample_i16(frame: usize) -> i16 {
        let phase = frame % Self::SAWTOOTH_PERIOD;
        let centered = i32::try_from(phase).expect("phase fits i32") - Self::SAWTOOTH_CENTER;
        i16::try_from(centered).expect("sawtooth centered value fits i16")
    }
}

impl PcmSource for SawtoothPcmFixture {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn total_byte_len(&self) -> Option<usize> {
        Some(self.bytes.len())
    }

    fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let Some(remaining) = self.bytes.get(offset..) else {
            return 0;
        };
        let read = remaining.len().min(buf.len());
        buf[..read].copy_from_slice(&remaining[..read]);
        read
    }
}
