//! WAV file generation helpers for tests.

use crate::signal_pcm::{Finite, SignalPcm, signal};

const WAV_HEADER_SIZE: usize = 44;

/// Create WAV with `signal` patten, sized exactly to `total_bytes`.
pub fn create_wav_exact_bytes<S: signal::SignalFn>(
    signal: S,
    sample_rate: u32,
    channels: u16,
    total_bytes: usize,
) -> Vec<u8> {
    let data_budget = total_bytes
        .checked_sub(WAV_HEADER_SIZE)
        .expect("WAV rendering needs room for a 44-byte header");

    let bytes_per_frame = size_of::<i16>() * channels as usize;
    let sample_count = data_budget / bytes_per_frame;

    create_wav(signal, sample_count, sample_rate, channels)
}

/// Create a WAV file with sine wave samples.
///
/// Parameters:
/// - `frames_count`: number of audio frames
/// - `sample_rate`: e.g. 44100
/// - `channels`: e.g. 2 for stereo
#[must_use]
pub fn create_test_wav(frames_count: usize, sample_rate: u32, channels: u16) -> Vec<u8> {
    create_wav(signal::SineWave(440.0), frames_count, sample_rate, channels)
}

/// Create a complete WAV file from a signal function and frame count.
pub fn create_wav<S: signal::SignalFn>(
    signal: S,
    total_frames: usize,
    sample_rate: u32,
    channels: u16,
) -> Vec<u8> {
    create_wav_from_signal(SignalPcm::new(
        signal,
        sample_rate,
        channels,
        Finite::new(total_frames),
    ))
}

/// Create a complete WAV file from a finite [`SignalPcm`].
pub fn create_wav_from_signal<S: signal::SignalFn>(pcm: SignalPcm<S>) -> Vec<u8> {
    let sample_rate = pcm.sample_rate();
    let channels = pcm.channels();

    let data = pcm.into_vec();
    let header = create_wav_header(sample_rate, channels, Some(data.len()));

    let mut buf = Vec::with_capacity(header.len() + data.len());

    buf.extend(header);
    buf.extend(data);

    buf
}

/// Build a 44-byte PCM WAV header.
///
/// - `data_size = None` → streaming header (sizes = `0xFFFFFFFF`).
/// - `data_size = Some(n)` → standard header with real sizes.
#[must_use]
pub fn create_wav_header(sample_rate: u32, channels: u16, data_size: Option<usize>) -> Vec<u8> {
    WavHeader::new(sample_rate, channels, data_size).into()
}

/// 44-byte PCM WAV header (RIFF/WAVE, 16-bit, `fmt ` + `data` chunks).
#[derive(Debug, Clone, Copy)]
pub struct WavHeader([u8; WAV_HEADER_SIZE]);

impl WavHeader {
    /// Build a header. `data_size = None` produces a streaming header (`0xFFFFFFFF`).
    #[must_use]
    pub fn new(sample_rate: u32, channels: u16, data_size: Option<usize>) -> Self {
        let bytes_per_sample: u16 = 2;
        let byte_rate = sample_rate * channels as u32 * bytes_per_sample as u32;
        let block_align = channels * bytes_per_sample;

        let (file_size_val, data_size_val) = data_size
            .map_or((0xFFFF_FFFFu32, 0xFFFF_FFFFu32), |size| {
                (36 + size as u32, size as u32)
            });

        let mut buf = [0u8; WAV_HEADER_SIZE];
        let mut cur = &mut buf[..];

        Self::write(&mut cur, *b"RIFF");
        Self::write(&mut cur, file_size_val.to_le_bytes());
        Self::write(&mut cur, *b"WAVE");
        Self::write(&mut cur, *b"fmt ");
        Self::write(&mut cur, 16u32.to_le_bytes());
        Self::write(&mut cur, 1u16.to_le_bytes());
        Self::write(&mut cur, channels.to_le_bytes());
        Self::write(&mut cur, sample_rate.to_le_bytes());
        Self::write(&mut cur, byte_rate.to_le_bytes());
        Self::write(&mut cur, block_align.to_le_bytes());
        Self::write(&mut cur, (bytes_per_sample * 8).to_le_bytes());
        Self::write(&mut cur, *b"data");
        Self::write(&mut cur, data_size_val.to_le_bytes());

        Self(buf)
    }

    /// Header size in bytes (always 44).
    #[must_use]
    pub const fn size(&self) -> usize {
        self.0.len()
    }

    #[expect(
        clippy::mut_mut,
        reason = "cursor-advancing pattern requires &mut &mut"
    )]
    fn write<const N: usize>(dst: &mut &mut [u8], bytes: [u8; N]) {
        let (head, tail) = std::mem::take(dst).split_at_mut(N);
        head.copy_from_slice(&bytes);
        *dst = tail;
    }
}

impl AsRef<[u8]> for WavHeader {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<WavHeader> for Vec<u8> {
    fn from(h: WavHeader) -> Self {
        h.0.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{
        kithara,
        signal_pcm::{Finite, Infinite, SignalPcm, signal},
        wav::{create_wav_from_signal, create_wav_header},
    };

    const WAV_HEADER_SIZE: usize = 44;

    #[kithara::test]
    fn wav_header_magic() {
        let src = SignalPcm::new(signal::Silence, 44100, 2, Finite::new(0));
        let buf = create_wav_from_signal(src);

        assert_eq!(buf.len(), WAV_HEADER_SIZE);
        assert_eq!(&buf[0..4], b"RIFF");
        assert_eq!(&buf[8..12], b"WAVE");
        assert_eq!(&buf[36..40], b"data");
    }

    #[kithara::test]
    fn wav_header_sample_rate() {
        let src = SignalPcm::new(signal::Silence, 48000, 1, Finite::new(1));
        let buf = create_wav_from_signal(src);

        let rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
        assert_eq!(rate, 48000);
    }

    #[kithara::test]
    fn finite_header_has_real_sizes() {
        let sample_rate = 44100;
        let pcm = SignalPcm::new(
            signal::Silence,
            sample_rate,
            2,
            Finite::from_duration(Duration::from_secs(1), sample_rate),
        );

        let data_size = sample_rate as u64 * 2 * 2;
        let file_size = 36 + data_size;

        let buf = create_wav_from_signal(pcm);
        let header = &buf[0..44];

        assert_eq!(
            u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
            file_size as u32
        );
        assert_eq!(
            u32::from_le_bytes([header[40], header[41], header[42], header[43]]),
            data_size as u32
        );
    }

    #[kithara::test]
    fn infinite_header_has_streaming_sizes() {
        let pcm = SignalPcm::new(signal::Silence, 44100, 2, Infinite);

        let sample_rate = pcm.sample_rate();
        let channels = pcm.channels();

        let buf = create_wav_header(sample_rate, channels, None);

        let header = &buf[0..44];

        assert_eq!(
            u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
            0xFFFF_FFFF
        );
        assert_eq!(
            u32::from_le_bytes([header[40], header[41], header[42], header[43]]),
            0xFFFF_FFFF
        );
    }

    #[kithara::test]
    fn create_wav_matches_first_samples() {
        // 2 stereo frames → 44 + 8 = 52 bytes; frame 0 at [44], frame 1 at [48].
        let pcm = SignalPcm::new(signal::Sawtooth, 44100, 2, Finite::new(2));
        let wav = create_wav_from_signal(pcm);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(i16::from_le_bytes([wav[44], wav[45]]), -32768);
        assert_eq!(i16::from_le_bytes([wav[48], wav[49]]), -32767);
    }

    #[kithara::test]
    fn create_wav_matches_pattern() {
        let channels = 2u16;
        let segment_count = 1;
        let segment_size = 8;
        let sample_rate = 44_100;

        let pcm = SignalPcm::new(
            signal::SawtoothDescending,
            sample_rate,
            channels,
            Finite::from_segments(segment_count, segment_size, channels),
        );

        let buf = create_wav_from_signal(pcm);

        let data = &buf[44..];

        assert_eq!(i16::from_le_bytes([data[0], data[1]]), 32767);
        assert_eq!(i16::from_le_bytes([data[4], data[5]]), 32766);
    }
}
