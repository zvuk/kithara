use super::Wave;

/// Interleaved 16-bit PCM held in memory.
pub struct Pcm {
    bytes: Vec<u8>,
    channels: u16,
    sample_rate: u32,
}

impl Pcm {
    /// Render `total_frames` of a waveform.
    #[must_use]
    pub fn new(sample_rate: u32, channels: u16, total_frames: usize, wave: Wave) -> Self {
        Self::from_fn(sample_rate, channels, total_frames, |frame| {
            wave.sample(frame, sample_rate)
        })
    }

    /// Number of interleaved channels.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// Render `total_frames` of a per-frame sample function, for a body no
    /// single [`Wave`] describes.
    #[must_use]
    pub fn from_fn<S: Fn(usize) -> i16>(
        sample_rate: u32,
        channels: u16,
        total_frames: usize,
        sample: S,
    ) -> Self {
        let lanes = usize::from(channels);
        let mut bytes = Vec::with_capacity(total_frames * lanes * size_of::<i16>());
        for frame in 0..total_frames {
            let value = sample(frame).to_le_bytes();
            for _ in 0..lanes {
                bytes.extend_from_slice(&value);
            }
        }
        Self {
            bytes,
            channels,
            sample_rate,
        }
    }

    /// Sample rate in Hz.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Validates and loads prepared interleaved PCM without synthesizing samples.
impl From<(u32, u16, Vec<u8>)> for Pcm {
    /// Panics for zero sample rate, zero channels, or an incomplete frame.
    fn from((sample_rate, channels, bytes): (u32, u16, Vec<u8>)) -> Self {
        assert!(sample_rate > 0, "PCM sample rate must be nonzero");
        assert!(channels > 0, "PCM channels must be nonzero");
        let frame_bytes = usize::from(channels) * size_of::<i16>();
        assert!(
            bytes.len().is_multiple_of(frame_bytes),
            "PCM must contain whole frames"
        );
        Self {
            bytes,
            channels,
            sample_rate,
        }
    }
}

impl From<Pcm> for Vec<u8> {
    fn from(pcm: Pcm) -> Self {
        pcm.bytes
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl kithara_encode::PcmSource for Pcm {
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

#[cfg(all(test, feature = "native", not(target_arch = "wasm32")))]
mod tests {
    use kithara_encode::PcmSource;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn byte_len_counts_every_channel_of_every_frame() {
        let pcm = Pcm::new(44_100, 2, 44_100, Wave::Silence);

        assert_eq!(pcm.total_byte_len(), Some(44_100 * 2 * 2));
    }

    #[kithara::test(native, flash(false))]
    fn a_frame_carries_the_same_sample_on_every_channel() {
        let pcm = Pcm::new(44_100, 2, 1, Wave::Sawtooth);
        let mut buf = [0u8; 4];

        assert_eq!(pcm.read_pcm_at(0, &mut buf), 4);
        assert_eq!(buf[..2], buf[2..]);
    }

    #[kithara::test(native, flash(false))]
    fn from_fn_renders_the_sample_it_is_given() {
        let pcm = Pcm::from_fn(44_100, 1, 3, |frame| {
            i16::try_from(frame).expect("small frame")
        });
        let bytes = Vec::from(pcm);

        assert_eq!(bytes, [0, 0, 1, 0, 2, 0]);
    }

    #[kithara::test(native, flash(false))]
    fn a_read_past_the_end_yields_nothing() {
        let pcm = Pcm::new(44_100, 1, 2, Wave::Sawtooth);
        let mut buf = [0xffu8; 8];

        assert_eq!(pcm.read_pcm_at(4, &mut buf), 0);
        assert_eq!(pcm.read_pcm_at(2, &mut buf), 2);
    }
}
