use std::time::Duration;

/// Saw-tooth period in frames. (u16 max value + 1)
pub const SAW_PERIOD: usize = 65536;
const MAX_FRAME_BYTES: usize = 32;

/// Built-in signal functions for test PCM generation.
pub mod signal {
    use super::SAW_PERIOD;
    use std::f64::consts::PI;

    /// Deterministic audio signal generator.
    ///
    /// Implementations must be pure functions: given the same `frame` and
    /// `sample_rate`, they must always return the same sample value.
    pub trait SignalFn: Send + 'static {
        /// Compute one 16-bit PCM sample at the given frame index.
        fn sample(&self, frame: usize, sample_rate: u32) -> i16;
    }

    /// Ascending saw-tooth: frame 0 → -32768, frame 65535 → 32767.
    #[derive(Debug)]
    pub struct Sawtooth;

    impl SignalFn for Sawtooth {
        fn sample(&self, frame: usize, _sample_rate: u32) -> i16 {
            ((frame % SAW_PERIOD) as i32 - 32768) as i16
        }
    }

    /// Descending saw-tooth: frame 0 → 32767, frame 65535 → -32768.
    #[derive(Debug)]
    pub struct SawtoothDescending;

    impl SignalFn for SawtoothDescending {
        fn sample(&self, frame: usize, _sample_rate: u32) -> i16 {
            (32767 - (frame % SAW_PERIOD) as i32) as i16
        }
    }

    /// Ascending saw-tooth with a half-period phase offset.
    #[derive(Debug)]
    pub struct SawtoothShifted;

    impl SignalFn for SawtoothShifted {
        fn sample(&self, frame: usize, _sample_rate: u32) -> i16 {
            (((frame + SAW_PERIOD / 2) % SAW_PERIOD) as i32 - 32768) as i16
        }
    }

    /// Pure sine wave at the given frequency in Hz.
    #[derive(Debug)]
    pub struct SineWave(pub f64);

    impl SignalFn for SineWave {
        fn sample(&self, frame: usize, sample_rate: u32) -> i16 {
            let t = frame as f64 / sample_rate as f64;
            (f64::sin(2.0 * PI * self.0 * t) * 32767.0) as i16
        }
    }

    /// Digital silence — all samples are zero.
    #[derive(Debug)]
    pub struct Silence;

    impl SignalFn for Silence {
        fn sample(&self, _frame: usize, _sample_rate: u32) -> i16 {
            0
        }
    }
}

/// Fixed-length duration measured in frames.
pub struct Finite {
    total_frames: usize,
}

impl Finite {
    /// Create from an exact frame count.
    #[must_use]
    pub const fn new(total_frames: usize) -> Self {
        Self { total_frames }
    }

    /// Create from a time duration and sample rate.
    #[must_use]
    pub const fn from_duration(duration: Duration, sample_rate: u32) -> Self {
        Self::new((duration.as_secs_f64() * sample_rate as f64) as usize)
    }

    /// Create from HLS-style segment layout (count, byte size per segment, channels).
    #[must_use]
    pub const fn from_segments(segment_count: usize, segment_size: usize, channels: u16) -> Self {
        let total_bytes = segment_count * segment_size;
        let bytes_per_frame = channels as usize * size_of::<i16>();
        let total_frames = total_bytes / bytes_per_frame;

        Self::new(total_frames)
    }
}

/// Unbounded duration marker for infinite signals.
pub struct Infinite;

/// PCM-first signal renderer used by fixture generators and WAV adapters.
pub struct SignalPcm<S: signal::SignalFn, F> {
    signal: S,
    sample_rate: u32,
    channels: u16,
    duration: F,
}

impl<S: signal::SignalFn, F> SignalPcm<S, F> {
    /// Create a new PCM renderer with the given signal, sample rate, channel count, and duration.
    #[must_use]
    pub const fn new(signal: S, sample_rate: u32, channels: u16, duration: F) -> Self {
        Self {
            signal,
            sample_rate,
            channels,
            duration,
        }
    }

    /// Sample rate in Hz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of audio channels.
    #[must_use]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    const fn bytes_per_frame(&self) -> usize {
        self.channels as usize * size_of::<u16>()
    }
}

impl<S: signal::SignalFn, F> SignalPcm<S, F> {
    /// Fill `buf` with PCM bytes starting at a PCM-relative byte offset,
    /// capped at `max_bytes` (pass `usize::MAX` for an infinite signal).
    pub(crate) fn render_pcm(&self, offset: usize, max_bytes: usize, buf: &mut [u8]) -> usize {
        if buf.is_empty() || offset >= max_bytes {
            return 0;
        }

        let bytes_per_frame = self.bytes_per_frame();
        let mut written = 0usize;
        let mut pos = offset;

        while written < buf.len() && pos < max_bytes {
            let frame = pos / bytes_per_frame;
            let byte_in_frame = pos % bytes_per_frame;
            let sample = self.signal.sample(frame, self.sample_rate);
            let sample_bytes = sample.to_le_bytes();
            let mut frame_buf = [0u8; MAX_FRAME_BYTES];
            for channel in 0..self.channels as usize {
                frame_buf[channel * 2] = sample_bytes[0];
                frame_buf[channel * 2 + 1] = sample_bytes[1];
            }

            let available = bytes_per_frame - byte_in_frame;
            let n = available.min(buf.len() - written).min(max_bytes - pos);
            buf[written..written + n].copy_from_slice(&frame_buf[byte_in_frame..byte_in_frame + n]);
            written += n;
            pos += n;
        }

        written
    }
}

impl<S: signal::SignalFn> SignalPcm<S, Finite> {
    /// Total number of audio frames.
    #[must_use]
    pub const fn total_frames(&self) -> usize {
        self.duration.total_frames
    }

    /// Total PCM byte length.
    #[must_use]
    pub const fn total_byte_len(&self) -> usize {
        self.total_frames() * self.bytes_per_frame()
    }

    /// Check whether `offset` is past EOF.
    #[must_use]
    pub fn is_past_eof(&self, offset: usize) -> bool {
        offset >= self.total_byte_len()
    }

    /// Render all PCM data into a `Vec<u8>`.
    pub fn into_vec(self) -> Vec<u8> {
        let total_bytes = self.total_byte_len();
        let mut bytes = vec![0u8; total_bytes];
        self.read_pcm_at(0, &mut bytes);
        bytes
    }

    /// Fill `buf` with PCM bytes starting at a PCM-relative byte offset.
    ///
    /// Returns the number of bytes written.
    pub fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        self.render_pcm(offset, self.total_byte_len(), buf)
    }
}

/// Marker for the length of a [`SignalPcm`] signal.
///
/// [`Finite`] is bounded; [`Infinite`] is unbounded.
pub trait DurationKind: Send + 'static {
    /// Returns the total PCM byte length for the given bytes-per-frame, or `None` for infinite.
    fn total_pcm_byte_len(&self, bytes_per_frame: usize) -> Option<usize>;
}

impl DurationKind for Finite {
    fn total_pcm_byte_len(&self, bytes_per_frame: usize) -> Option<usize> {
        Some(self.total_frames * bytes_per_frame)
    }
}

impl DurationKind for Infinite {
    fn total_pcm_byte_len(&self, _bytes_per_frame: usize) -> Option<usize> {
        None
    }
}

impl<S: signal::SignalFn, F: DurationKind> SignalPcm<S, F> {
    /// Total PCM byte length, or `None` for infinite signals.
    pub fn total_pcm_byte_len(&self) -> Option<usize> {
        self.duration.total_pcm_byte_len(self.bytes_per_frame())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal_pcm::signal::SignalFn;
    use crate::signal_pcm::{SignalPcm, signal};

    #[test]
    fn pcm_finite_len() {
        let sample_rate = 44100;
        let pcm = SignalPcm::new(
            signal::Silence,
            sample_rate,
            2,
            Finite::from_duration(Duration::from_secs(1), sample_rate),
        );

        assert_eq!(pcm.total_byte_len(), 44100 * 2 * 2);
    }

    #[test]
    fn pcm_partial_frame_read() {
        let pcm = SignalPcm::new(signal::Sawtooth, 44100, 1, Finite::new(2));
        let mut buf = [0u8; 4];
        assert_eq!(pcm.read_pcm_at(0, &mut buf), 4);
        assert_eq!(i16::from_le_bytes([buf[0], buf[1]]), -32768);
        assert_eq!(i16::from_le_bytes([buf[2], buf[3]]), -32767);
    }

    #[test]
    fn sawtooth_descending() {
        let pcm = SignalPcm::new(signal::SawtoothDescending, 44100, 1, Finite::new(1));
        let mut buf = [0u8; 2];
        assert_eq!(pcm.read_pcm_at(0, &mut buf), 2);
        assert_eq!(i16::from_le_bytes(buf), 32767);
    }

    #[test]
    fn sine_first_sample_is_zero() {
        let pcm = SignalPcm::new(signal::SineWave(440.0), 44100, 1, Finite::new(1));
        let mut buf = [0u8; 2];
        assert_eq!(pcm.read_pcm_at(0, &mut buf), 2);
        assert_eq!(i16::from_le_bytes(buf), 0);
    }

    #[test]
    fn silence_all_zeros() {
        let sample_rate = 44100;
        let pcm = SignalPcm::new(
            signal::Silence,
            sample_rate,
            2,
            Finite::from_duration(Duration::from_millis(10), sample_rate),
        );

        let pcm_bytes = 44100 * 2 * 2 / 100;
        let mut buf = vec![0xFFu8; pcm_bytes];
        assert_eq!(pcm.read_pcm_at(0, &mut buf), pcm_bytes);
        assert!(buf.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn stereo_duplicates_channels() {
        let pcm = SignalPcm::new(signal::Sawtooth, 44100, 2, Finite::new(1));
        let mut buf = [0u8; 4];

        pcm.read_pcm_at(0, &mut buf);

        assert_eq!(buf[0], buf[2]);
        assert_eq!(buf[1], buf[3]);
    }

    #[test]
    fn custom_signal_fn() {
        struct Constant(i16);

        impl SignalFn for Constant {
            fn sample(&self, _frame: usize, _sample_rate: u32) -> i16 {
                self.0
            }
        }

        let src = SignalPcm::new(Constant(1000), 44100, 1, Finite::new(1));
        let mut buf = [0u8; 2];

        src.read_pcm_at(0, &mut buf);

        assert_eq!(i16::from_le_bytes(buf), 1000);
    }
}
