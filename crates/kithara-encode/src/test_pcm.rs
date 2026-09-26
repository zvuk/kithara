#[cfg(feature = "ffmpeg")]
use std::mem::size_of;

use crate::{PcmSource, consts};

pub(crate) struct TestPcm {
    bytes: Vec<u8>,
    channels: u16,
    sample_rate: u32,
}

impl TestPcm {
    pub(crate) fn from_bytes(bytes: Vec<u8>, sample_rate: u32, channels: u16) -> Self {
        Self {
            bytes,
            channels,
            sample_rate,
        }
    }

    #[cfg(feature = "ffmpeg")]
    pub(crate) fn samples_f32(&self) -> Vec<f32> {
        self.bytes
            .chunks_exact(size_of::<i16>())
            .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / consts::I16_SCALE)
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
