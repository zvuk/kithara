use std::{
    io::{self, Cursor, ErrorKind, Read, Seek, SeekFrom},
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, NotReadyCause, PendingReason, SourcePhase,
    StreamPending,
};
use kithara_test_fixtures::assets::rhythm_mp3_deck_a_120bpm_48k;
use kithara_test_utils::kithara;

use super::{DecoderBackend, DecoderConfig, DecoderFactory};
use crate::{
    DecoderChunkOutcome, DecoderSeekOutcome,
    test_pools::{TestPools, pools},
    traits::Decoder,
};

struct Consts;

impl Consts {
    /// One beat of the 120 BPM fixture at 48 kHz.
    const BEAT_FRAMES: usize = 24_000;
    const BEAT_ENERGY_TOLERANCE: f64 = 0.05;
    const MPEG_FRAME: u64 = 1_152;
    const ONSET_WINDOW_FRAMES: usize = 480;
    const READY_PREFIX: u64 = 32 * 1024;
    const SEEK: Duration = Duration::from_secs(8);
}

/// Bytes past `ready` answer the way a streamed source answers a range that is still downloading.
struct Download {
    inner: Cursor<Vec<u8>>,
    ready: Arc<AtomicU64>,
}

impl Read for Download {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.inner.position();
        let len = u64::try_from(self.inner.get_ref().len()).expect("fixture length fits u64");
        let want = u64::try_from(buf.len()).expect("request length fits u64");
        if pos.saturating_add(want).min(len) > self.ready.load(Ordering::Acquire) {
            return Err(io::Error::new(
                ErrorKind::Interrupted,
                StreamPending::new(
                    PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted),
                    pos,
                    buf.len(),
                    Some(len),
                    SourcePhase::Waiting,
                    0,
                    false,
                ),
            ));
        }
        self.inner.read(buf)
    }
}

impl Seek for Download {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Mono PCM on the absolute track timeline, and the byte a seek reported landing on.
struct Decoded {
    landed_byte: Option<u64>,
    start: usize,
    mono: Vec<f32>,
}

impl Decoded {
    fn end(&self) -> usize {
        self.start + self.mono.len()
    }

    /// RMS of each whole beat, centred on its pulse so a sub-beat offset between decoders
    /// cannot move a pulse into the neighbouring window.
    fn beat_energy(&self, from: usize, to: usize) -> Vec<f64> {
        let half = Consts::BEAT_FRAMES / 2;
        let first = from.saturating_sub(half).div_ceil(Consts::BEAT_FRAMES);
        (first..)
            .map(|beat| beat * Consts::BEAT_FRAMES + half)
            .take_while(|window| window + Consts::BEAT_FRAMES <= to)
            .map(|window| rms(&self.mono[window - self.start..][..Consts::BEAT_FRAMES]))
            .collect()
    }

    /// Absolute frames where the pulse train rises above half its peak level.
    fn onsets(&self) -> Vec<usize> {
        let levels: Vec<f64> = self
            .mono
            .chunks_exact(Consts::ONSET_WINDOW_FRAMES)
            .map(rms)
            .collect();
        let threshold = levels.iter().copied().fold(0.0_f64, f64::max) / 2.0;
        levels
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair[0] <= threshold && pair[1] > threshold)
            .map(|(index, _)| self.start + (index + 1) * Consts::ONSET_WINDOW_FRAMES)
            .collect()
    }
}

fn rms(samples: &[f32]) -> f64 {
    let energy: f64 = samples
        .iter()
        .map(|&sample| f64::from(sample).powi(2))
        .sum();
    let count = f64::from(u32::try_from(samples.len()).expect("window length fits u32"));
    (energy / count).sqrt()
}

fn mp3() -> MediaInfo {
    MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Mp3))
        .maybe_container(Some(ContainerFormat::MpegAudio))
        .build()
}

/// Lets the rest of the download arrive; a decoder that parks once it has is stuck, not waiting.
fn release(ready: &AtomicU64) {
    let arrived = ready.swap(u64::MAX, Ordering::AcqRel) == u64::MAX;
    assert!(!arrived, "the decoder parks on bytes that have all arrived");
}

/// Decodes to EOF the way the pipeline drives a decoder: an interrupted seek is applied again
/// once the source has the bytes, and a parked read is retried.
fn decode(mut decoder: Box<dyn Decoder>, seek: Option<Duration>, ready: &AtomicU64) -> Decoded {
    let mut start = None;
    let mut landed = 0;
    let mut landed_byte = None;
    if let Some(position) = seek {
        let landed_frame = loop {
            match decoder.seek(position) {
                Ok(DecoderSeekOutcome::Landed {
                    landed_frame,
                    landed_byte: byte,
                    ..
                }) => {
                    landed_byte = byte;
                    break landed_frame;
                }
                Ok(outcome) => panic!("the seek lands inside the track: {outcome:?}"),
                Err(error) if error.is_interrupted() => release(ready),
                Err(error) => panic!("seek failed: {error}"),
            }
        };
        landed = usize::try_from(landed_frame).expect("landed frame fits usize");
    }
    let mut mono = Vec::new();
    loop {
        match decoder.next_chunk().expect("decode") {
            DecoderChunkOutcome::Chunk(chunk) => {
                let offset = usize::try_from(chunk.meta.frame_offset).expect("offset fits usize");
                let first = *start.get_or_insert(offset);
                assert_eq!(
                    offset,
                    first + mono.len(),
                    "decoded PCM must stay contiguous"
                );
                let channels = usize::from(chunk.meta.spec.channels);
                let scale = f32::from(chunk.meta.spec.channels);
                mono.extend(
                    chunk
                        .samples
                        .chunks_exact(channels)
                        .map(|frame| frame.iter().sum::<f32>() / scale),
                );
            }
            DecoderChunkOutcome::Pending(_) => release(ready),
            DecoderChunkOutcome::Eof => break,
        }
    }
    Decoded {
        landed_byte,
        start: start.unwrap_or(landed),
        mono,
    }
}

fn reference(bytes: &[u8], seek: Option<Duration>) -> Decoded {
    let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
        DecoderConfig::builder()
            .backend(DecoderBackend::Symphonia)
            .pools(pools())
            .build();
    let decoder =
        DecoderFactory::create_from_media_info(Cursor::new(bytes.to_vec()), &mp3(), config)
            .expect("symphonia MP3 decoder");
    decode(decoder, seek, &AtomicU64::new(u64::MAX))
}

fn streamed_apple(bytes: &[u8], seek: Option<Duration>) -> (Decoded, bool) {
    let total = u64::try_from(bytes.len()).expect("fixture length fits u64");
    let ready = Arc::new(AtomicU64::new(Consts::READY_PREFIX));
    let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
        DecoderConfig::builder()
            .backend(DecoderBackend::Apple)
            .byte_len_handle(Arc::new(AtomicU64::new(total)))
            .pools(pools())
            .build();
    let source = Download {
        inner: Cursor::new(bytes.to_vec()),
        ready: Arc::clone(&ready),
    };
    let decoder = DecoderFactory::create_from_media_info(source, &mp3(), config)
        .expect("streamed Apple MP3 decoder");
    let decoded = decode(decoder, seek, &ready);
    (decoded, ready.load(Ordering::Acquire) == u64::MAX)
}

/// A streamed MP3 that the decoder reaches before its bytes do must play on once they arrive:
/// the same span, the same pulse per beat, and the same beat grid as Symphonia on the same file.
#[kithara::test]
#[case::seek_past_the_download(Some(Consts::SEEK))]
#[case::read_up_to_the_download(None)]
fn streamed_apple_mp3_resumes_where_the_download_arrives(#[case] seek: Option<Duration>) {
    let bytes = rhythm_mp3_deck_a_120bpm_48k().bytes();
    let expected = reference(bytes, seek);
    let (decoded, parked) = streamed_apple(bytes, seek);

    assert!(
        parked,
        "the decoder must reach bytes that are not there yet"
    );
    if seek.is_some() {
        let total = u64::try_from(bytes.len()).expect("fixture length fits u64");
        assert!(
            decoded.landed_byte.is_some_and(|byte| byte < total),
            "a seek must name the byte it resumes from so the stream cursor follows it, got {:?}",
            decoded.landed_byte
        );
    }
    let tolerance = usize::try_from(2 * Consts::MPEG_FRAME).expect("tolerance fits usize");
    assert!(
        decoded.end().abs_diff(expected.end()) <= tolerance,
        "the track ends at frame {} where symphonia ends at {}",
        decoded.end(),
        expected.end()
    );
    assert!(
        decoded.start.abs_diff(expected.start) <= tolerance,
        "decoding resumes at frame {} where symphonia resumes at {}",
        decoded.start,
        expected.start
    );

    let from = decoded.start.max(expected.start);
    let to = decoded.end().min(expected.end());
    let energy = decoded.beat_energy(from, to);
    let expected_energy = expected.beat_energy(from, to);
    assert!(!expected_energy.is_empty(), "the span holds whole beats");
    for (beat, (got, want)) in energy.iter().zip(&expected_energy).enumerate() {
        assert!(
            (got - want).abs() <= want * Consts::BEAT_ENERGY_TOLERANCE,
            "beat {beat} carries RMS {got} where symphonia carries {want}"
        );
    }

    let onsets = decoded.onsets();
    let expected_onsets = expected.onsets();
    assert_eq!(
        onsets.len(),
        expected_onsets.len(),
        "every pulse is decoded once"
    );
    for (got, want) in onsets.iter().zip(&expected_onsets) {
        assert!(
            got.abs_diff(*want) <= Consts::ONSET_WINDOW_FRAMES,
            "a pulse lands at frame {got} where symphonia puts it at {want}"
        );
    }
    for pair in onsets.windows(2) {
        assert!(
            (pair[1] - pair[0]).abs_diff(Consts::BEAT_FRAMES) <= Consts::ONSET_WINDOW_FRAMES,
            "pulses {} frames apart break the 120 BPM grid",
            pair[1] - pair[0]
        );
    }
}
