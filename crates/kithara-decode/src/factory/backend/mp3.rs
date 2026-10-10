use std::{
    io::{self, Cursor, ErrorKind, Read, Seek, SeekFrom},
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, NotReadyCause, PendingReason, SourcePhase,
    StreamPending,
};
use kithara_test_fixtures::{assets::rhythm_mp3_deck_a_120bpm_48k, signal::rms};
use kithara_test_utils::kithara;

use crate::{
    DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory, DecoderSeekOutcome,
    test_pools::{TestPools, pools},
    traits::Decoder,
};

mod consts {
    use super::Duration;
    /// One beat of the 120 BPM fixture at 48 kHz.
    pub(super) const BEAT_FRAMES: usize = 24_000;
    pub(super) const BEAT_ENERGY_TOLERANCE: f64 = 0.05;
    pub(super) const MPEG_FRAME: u64 = 1_152;
    pub(super) const ONSET_WINDOW_FRAMES: usize = 480;
    pub(super) const READY_PREFIX: u64 = 32 * 1024;
    pub(super) const SEEK: Duration = Duration::from_secs(8);
}

#[kithara::test]
#[case(false)]
#[case(true)]
fn mp3_in_wave_matches_elementary_audio_and_seeks_in_file_coordinates(#[case] tagged: bool) {
    let audio = rhythm_mp3_deck_a_120bpm_48k().bytes();
    let mut wave = Vec::new();
    if tagged {
        let len = u32::try_from(audio.len()).expect("fixture fits an ID3 tag size");
        // ID3v2 sizes are syncsafe: seven payload bits per byte.
        let syncsafe = (len & 0x0FE0_0000) << 3
            | (len & 0x001F_C000) << 2
            | (len & 0x0000_3F80) << 1
            | (len & 0x0000_007F);
        wave.extend_from_slice(&[b'I', b'D', b'3', 4, 0, 0]);
        wave.extend_from_slice(&syncsafe.to_be_bytes());
        wave.extend_from_slice(audio);
    }
    let metadata_prefix = wave.clone();
    let origin = wave.len();
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(
        &u32::try_from(audio.len() + 36)
            .expect("RIFF length")
            .to_le_bytes(),
    );
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16u32.to_le_bytes());
    wave.extend_from_slice(&0x55u16.to_le_bytes());
    wave.extend_from_slice(&2u16.to_le_bytes());
    wave.extend_from_slice(&48_000u32.to_le_bytes());
    wave.extend_from_slice(&16_000u32.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&0u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(
        &u32::try_from(audio.len())
            .expect("payload length")
            .to_le_bytes(),
    );
    wave.extend_from_slice(audio);
    let config = || {
        DecoderConfig::<kithara_resampler::NoResamplerBackend, TestPools>::builder()
            .backend(DecoderBackend::Apple)
            .pools(pools())
            .build()
    };
    let mut plain =
        DecoderFactory::create_with_probe(Cursor::new(audio.to_vec()), Some("mp3"), config())
            .expect("elementary MP3");
    let mut wrapped = DecoderFactory::create_from_media_info(
        Cursor::new(wave),
        &MediaInfo::builder().container(ContainerFormat::Wav).build(),
        config(),
    )
    .expect("MP3 in WAV");
    let drain = |decoder: &mut dyn Decoder| {
        let mut pcm = Vec::new();
        loop {
            match decoder.next_chunk().expect("decode PCM") {
                DecoderChunkOutcome::Chunk(chunk) => pcm.extend_from_slice(&chunk.samples),
                DecoderChunkOutcome::Pending(_) => panic!("complete fixture must not stall"),
                DecoderChunkOutcome::Eof => return pcm,
            }
        }
    };
    let reference = drain(&mut *plain);
    assert_eq!(
        reference,
        drain(&mut *wrapped),
        "the carrier and its metadata do not alter audio"
    );
    if tagged {
        let mut elementary = metadata_prefix;
        elementary.extend_from_slice(audio);
        let mut tagged_decoder =
            DecoderFactory::create_with_probe(Cursor::new(elementary), Some("mp3"), config())
                .expect("MP3 with audio-looking metadata");
        assert_eq!(
            reference,
            drain(&mut *tagged_decoder),
            "metadata bytes are never MPEG frames"
        );
    }
    let outcome = wrapped
        .seek(Duration::from_secs(2))
        .expect("seek in WAV payload");
    let DecoderSeekOutcome::Landed {
        landed_byte: Some(byte),
        ..
    } = outcome
    else {
        panic!("seek must expose a source byte position");
    };
    assert!(byte >= u64::try_from(origin + 44).expect("payload starts after RIFF headers"));
    assert!(
        !drain(&mut *wrapped).is_empty(),
        "seek resumes decoding the payload"
    );
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
        let half = consts::BEAT_FRAMES / 2;
        let first = from.saturating_sub(half).div_ceil(consts::BEAT_FRAMES);
        (first..)
            .map(|beat| beat * consts::BEAT_FRAMES + half)
            .take_while(|window| window + consts::BEAT_FRAMES <= to)
            .map(|window| f64::from(rms(&self.mono[window - self.start..][..consts::BEAT_FRAMES])))
            .collect()
    }

    /// Absolute frames where the pulse train rises above half its peak level.
    fn onsets(&self) -> Vec<usize> {
        let levels: Vec<f64> = self
            .mono
            .chunks_exact(consts::ONSET_WINDOW_FRAMES)
            .map(|window| f64::from(rms(window)))
            .collect();
        let threshold = levels.iter().copied().fold(0.0_f64, f64::max) / 2.0;
        levels
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair[0] <= threshold && pair[1] > threshold)
            .map(|(index, _)| self.start + (index + 1) * consts::ONSET_WINDOW_FRAMES)
            .collect()
    }
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
    let ready = Arc::new(AtomicU64::new(consts::READY_PREFIX));
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
#[case::seek_past_the_download(Some(consts::SEEK))]
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
    let tolerance = usize::try_from(2 * consts::MPEG_FRAME).expect("tolerance fits usize");
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
            (got - want).abs() <= want * consts::BEAT_ENERGY_TOLERANCE,
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
            got.abs_diff(*want) <= consts::ONSET_WINDOW_FRAMES,
            "a pulse lands at frame {got} where symphonia puts it at {want}"
        );
    }
    for pair in onsets.windows(2) {
        assert!(
            (pair[1] - pair[0]).abs_diff(consts::BEAT_FRAMES) <= consts::ONSET_WINDOW_FRAMES,
            "pulses {} frames apart break the 120 BPM grid",
            pair[1] - pair[0]
        );
    }
}
