use std::time::Duration;

/// Saw-tooth period in frames. (u16 max value + 1)
pub const SAW_PERIOD: usize = 65536;
const MAX_FRAME_BYTES: usize = 32;

/// Frequency interpolation mode for [`signal::Sweep`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepMode {
    Linear,
    Log,
}

/// Built-in signal functions for test PCM generation.
pub mod signal {
    use std::f64::consts::PI;

    use super::{SAW_PERIOD, SweepMode};

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

    /// Phase-continuous chirp with analytically computed phase.
    #[derive(Debug)]
    pub struct Sweep {
        pub start_hz: f64,
        pub end_hz: f64,
        pub total_frames: usize,
        pub mode: SweepMode,
    }

    impl Sweep {
        #[must_use]
        pub fn new(start_hz: f64, end_hz: f64, total_frames: usize, mode: SweepMode) -> Self {
            assert!(start_hz.is_finite() && start_hz > 0.0);
            assert!(end_hz.is_finite() && end_hz > 0.0);
            assert!(total_frames > 0);
            if matches!(mode, SweepMode::Log) {
                assert!(start_hz != end_hz);
            }

            Self {
                start_hz,
                end_hz,
                total_frames,
                mode,
            }
        }

        fn phase(&self, frame: usize, sample_rate: u32) -> f64 {
            let t = frame as f64 / sample_rate as f64;
            let duration_secs = self.total_frames as f64 / sample_rate as f64;
            match self.mode {
                SweepMode::Linear => {
                    let slope = (self.end_hz - self.start_hz) / (2.0 * duration_secs);
                    2.0 * PI * (self.start_hz * t + slope * t * t)
                }
                SweepMode::Log => {
                    let k = f64::ln(self.end_hz / self.start_hz) / duration_secs;
                    2.0 * PI * self.start_hz * (f64::exp(k * t) - 1.0) / k
                }
            }
        }
    }

    impl SignalFn for Sweep {
        fn sample(&self, frame: usize, sample_rate: u32) -> i16 {
            if frame >= self.total_frames {
                return 0;
            }

            (f64::sin(self.phase(frame, sample_rate)) * 32_767.0) as i16
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

/// Normalized signal length used by PCM renderers and request parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalLength {
    /// Finite signal with an exact frame count.
    Finite { total_frames: usize },
    /// Unbounded signal with no EOF.
    Infinite,
}

impl SignalLength {
    /// Create from an exact frame count.
    #[must_use]
    pub const fn from_frames(total_frames: usize) -> Self {
        Self::Finite { total_frames }
    }

    /// Create from a time duration and sample rate.
    #[must_use]
    pub fn from_duration(duration: Duration, sample_rate: u32) -> Self {
        Self::from_frames((duration.as_secs_f64() * sample_rate as f64) as usize)
    }

    /// Create from an HLS-style segment layout (count, byte size per segment, channels).
    #[must_use]
    pub const fn from_segments(segment_count: usize, segment_size: usize, channels: u16) -> Self {
        let total_bytes = segment_count * segment_size;
        let bytes_per_frame = channels as usize * size_of::<i16>();
        let total_frames = total_bytes / bytes_per_frame;

        Self::from_frames(total_frames)
    }

    /// Total number of frames, or `None` for unbounded signals.
    #[must_use]
    pub const fn total_frames(self) -> Option<usize> {
        match self {
            Self::Finite { total_frames } => Some(total_frames),
            Self::Infinite => None,
        }
    }

    /// Total PCM byte length, or `None` for unbounded signals.
    #[must_use]
    pub const fn total_pcm_byte_len(self, channels: u16) -> Option<usize> {
        match self {
            Self::Finite { total_frames } => {
                total_frames.checked_mul(channels as usize * size_of::<u16>())
            }
            Self::Infinite => None,
        }
    }
}

/// Backward-compatible finite length wrapper used by existing tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub fn from_duration(duration: Duration, sample_rate: u32) -> Self {
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

    /// Total number of frames.
    #[must_use]
    pub const fn total_frames(self) -> usize {
        self.total_frames
    }
}

impl From<Finite> for SignalLength {
    fn from(value: Finite) -> Self {
        Self::from_frames(value.total_frames())
    }
}

/// Backward-compatible infinite length marker used by existing tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Infinite;

impl From<Infinite> for SignalLength {
    fn from(_: Infinite) -> Self {
        Self::Infinite
    }
}

/// PCM-first signal renderer used by fixture generators and WAV adapters.
pub struct SignalPcm<S: signal::SignalFn> {
    signal: S,
    sample_rate: u32,
    channels: u16,
    length: SignalLength,
}

impl<S: signal::SignalFn> SignalPcm<S> {
    /// Create a new PCM renderer with the given signal, sample rate, channel count, and duration.
    #[must_use]
    pub fn new<L: Into<SignalLength>>(
        signal: S,
        sample_rate: u32,
        channels: u16,
        length: L,
    ) -> Self {
        Self {
            signal,
            sample_rate,
            channels,
            length: length.into(),
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

    /// Normalized signal length.
    #[must_use]
    pub const fn length(&self) -> SignalLength {
        self.length
    }

    const fn bytes_per_frame(&self) -> usize {
        self.channels as usize * size_of::<u16>()
    }

    /// Total number of audio frames, or `None` for infinite signals.
    #[must_use]
    pub const fn total_frames(&self) -> Option<usize> {
        self.length.total_frames()
    }

    /// Total PCM byte length, or `None` for infinite signals.
    #[must_use]
    pub const fn total_byte_len(&self) -> Option<usize> {
        self.length.total_pcm_byte_len(self.channels)
    }

    /// Total PCM byte length, or `None` for infinite signals.
    #[must_use]
    pub const fn total_pcm_byte_len(&self) -> Option<usize> {
        self.total_byte_len()
    }

    /// Check whether `offset` is past EOF for finite signals.
    #[must_use]
    pub fn is_past_eof(&self, offset: usize) -> bool {
        self.total_byte_len().is_some_and(|total| offset >= total)
    }

    /// Fill `buf` with PCM bytes starting at a PCM-relative byte offset.
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

    /// Render all PCM data into a `Vec<u8>`.
    pub fn into_vec(self) -> Vec<u8> {
        let total_bytes = self
            .total_byte_len()
            .expect("rendering a full Vec requires a finite signal length");
        let mut bytes = vec![0u8; total_bytes];
        self.read_pcm_at(0, &mut bytes);
        bytes
    }

    /// Fill `buf` with PCM bytes starting at a PCM-relative byte offset.
    ///
    /// Returns the number of bytes written.
    pub fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        self.render_pcm(offset, self.total_byte_len().unwrap_or(usize::MAX), buf)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<S: signal::SignalFn + Sync> kithara_encode::PcmSource for SignalPcm<S> {
    fn sample_rate(&self) -> u32 {
        Self::sample_rate(self)
    }

    fn channels(&self) -> u16 {
        Self::channels(self)
    }

    fn total_byte_len(&self) -> Option<usize> {
        Self::total_byte_len(self)
    }

    fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        Self::read_pcm_at(self, offset, buf)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;
    use crate::signal_pcm::{SignalLength, SignalPcm, SweepMode, signal, signal::SignalFn};

    const SAMPLE_RATE: u32 = 48_000;

    fn read_mono_samples<S: SignalFn>(pcm: &SignalPcm<S>) -> Vec<i16> {
        let mut bytes = vec![0u8; pcm.total_byte_len().expect("finite test signal")];
        assert_eq!(pcm.read_pcm_at(0, &mut bytes), bytes.len());
        bytes
            .chunks_exact(pcm.channels() as usize * 2)
            .map(|frame| i16::from_le_bytes([frame[0], frame[1]]))
            .collect()
    }

    fn read_overlap<S: SignalFn>(pcm: &SignalPcm<S>, offset: usize, len: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; len];
        let written = pcm.read_pcm_at(offset, &mut bytes);
        bytes.truncate(written);
        bytes
    }

    fn zero_crossings(samples: &[i16]) -> usize {
        let mut crossings = 0usize;
        let mut prev_sign = 0i8;

        for &sample in samples {
            let sign = if sample > 0 {
                1
            } else if sample < 0 {
                -1
            } else {
                0
            };

            if sign == 0 {
                continue;
            }

            if prev_sign != 0 && sign != prev_sign {
                crossings += 1;
            }
            prev_sign = sign;
        }

        crossings
    }

    fn estimate_frequency(samples: &[i16], sample_rate: u32) -> f64 {
        let duration_secs = samples.len() as f64 / sample_rate as f64;
        zero_crossings(samples) as f64 / (2.0 * duration_secs)
    }

    fn window(samples: &[i16], range: Range<usize>) -> &[i16] {
        &samples[range]
    }

    #[test]
    fn pcm_finite_len() {
        let sample_rate = 44100;
        let pcm = SignalPcm::new(
            signal::Silence,
            sample_rate,
            2,
            Finite::from_duration(Duration::from_secs(1), sample_rate),
        );

        assert_eq!(pcm.total_byte_len(), Some(44100 * 2 * 2));
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

    #[test]
    fn infinite_signal_has_no_known_len() {
        let pcm = SignalPcm::new(signal::Silence, 44_100, 2, Infinite);

        assert_eq!(pcm.length(), SignalLength::Infinite);
        assert_eq!(pcm.total_frames(), None);
        assert_eq!(pcm.total_byte_len(), None);
        assert!(!pcm.is_past_eof(usize::MAX));
    }

    #[test]
    fn sweep_first_sample_is_zero() {
        let sweep = signal::Sweep::new(100.0, 8_000.0, SAMPLE_RATE as usize, SweepMode::Linear);
        let pcm = SignalPcm::new(sweep, SAMPLE_RATE, 1, Finite::new(SAMPLE_RATE as usize));
        let mut buf = [0u8; 2];

        assert_eq!(pcm.read_pcm_at(0, &mut buf), 2);
        assert_eq!(i16::from_le_bytes(buf), 0);
    }

    #[test]
    fn sweep_zero_crossing_density_increases_over_time() {
        let total_frames = (SAMPLE_RATE * 2) as usize;
        let sweep = signal::Sweep::new(100.0, 6_400.0, total_frames, SweepMode::Linear);
        let pcm = SignalPcm::new(sweep, SAMPLE_RATE, 1, Finite::new(total_frames));
        let samples = read_mono_samples(&pcm);
        let window_size = 4_096;
        let early = zero_crossings(window(&samples, 2_048..2_048 + window_size));
        let middle = zero_crossings(window(&samples, 32_768..32_768 + window_size));
        let late = zero_crossings(window(&samples, 72_000..72_000 + window_size));

        assert!(early < middle);
        assert!(middle < late);
    }

    #[test]
    fn sweep_has_no_discontinuity_across_chunk_boundaries() {
        let total_frames = (SAMPLE_RATE * 2) as usize;
        let pcm = SignalPcm::new(
            signal::Sweep::new(100.0, 8_000.0, total_frames, SweepMode::Linear),
            SAMPLE_RATE,
            1,
            Finite::new(total_frames),
        );
        let full = read_overlap(&pcm, 0, pcm.total_byte_len().expect("finite signal"));
        let split_at = 17_531;
        let mut stitched = read_overlap(&pcm, 0, split_at);
        stitched.extend(read_overlap(
            &pcm,
            split_at,
            full.len().saturating_sub(split_at),
        ));

        assert_eq!(stitched, full);
    }

    #[test]
    fn sweep_frequency_near_end_matches_target() {
        let total_frames = SAMPLE_RATE as usize;
        let pcm = SignalPcm::new(
            signal::Sweep::new(100.0, 4_000.0, total_frames, SweepMode::Linear),
            SAMPLE_RATE,
            1,
            Finite::new(total_frames),
        );
        let samples = read_mono_samples(&pcm);
        let tail = window(&samples, total_frames - 1_024..total_frames);
        let tail_frequency = estimate_frequency(tail, SAMPLE_RATE);

        assert!((tail_frequency - 4_000.0).abs() < 220.0);
    }

    #[test]
    fn log_sweep_midpoint_frequency_matches_geometric_mean() {
        let total_frames = (SAMPLE_RATE * 2) as usize;
        let pcm = SignalPcm::new(
            signal::Sweep::new(100.0, 1_000.0, total_frames, SweepMode::Log),
            SAMPLE_RATE,
            1,
            Finite::new(total_frames),
        );
        let samples = read_mono_samples(&pcm);
        let midpoint = total_frames / 2;
        let estimate = estimate_frequency(
            window(&samples, midpoint - 2_048..midpoint + 2_048),
            SAMPLE_RATE,
        );
        let expected = 100.0 * f64::sqrt(10.0);

        assert!((estimate - expected).abs() < 25.0);
    }

    #[test]
    fn sweep_stereo_duplicates_channels() {
        let total_frames = SAMPLE_RATE as usize / 10;
        let pcm = SignalPcm::new(
            signal::Sweep::new(100.0, 2_000.0, total_frames, SweepMode::Linear),
            SAMPLE_RATE,
            2,
            Finite::new(total_frames),
        );
        let bytes = read_overlap(&pcm, 0, 4 * 32);

        for frame in bytes.chunks_exact(4) {
            assert_eq!(&frame[..2], &frame[2..4]);
        }
    }

    #[test]
    fn sweep_reads_are_idempotent_across_offsets() {
        let total_frames = (SAMPLE_RATE * 2) as usize;
        let pcm = SignalPcm::new(
            signal::Sweep::new(100.0, 8_000.0, total_frames, SweepMode::Linear),
            SAMPLE_RATE,
            2,
            Finite::new(total_frames),
        );
        let full = read_overlap(&pcm, 0, pcm.total_byte_len().expect("finite signal"));
        let offset = 7_513;
        let overlap = read_overlap(&pcm, offset, full.len() - offset);

        assert_eq!(&full[offset..], overlap.as_slice());
    }

    #[test]
    fn sweep_large_offsets_remain_deterministic() {
        let total_frames = 1_250_000usize;
        let pcm = SignalPcm::new(
            signal::Sweep::new(100.0, 10_000.0, total_frames, SweepMode::Log),
            SAMPLE_RATE,
            1,
            Finite::new(total_frames),
        );
        let offset = 2 * 567_891;
        let first = read_overlap(&pcm, offset, 4_096);
        let second = read_overlap(&pcm, offset, 4_096);

        assert_eq!(first, second);
    }
}
