use std::io::Cursor;

use kithara_decode::{Decoder, DecoderChunkOutcome, DecoderConfig, DecoderFactory};
use kithara_platform::time::Duration;
use kithara_signal::AudioChunk;
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
#[cfg(feature = "apple")]
use kithara_test_fixtures::assets::{alac_silence_1s, flac_unknown_length_saw_1s};
use kithara_test_fixtures::{assets::sine_wav_a440_full_scale_2s, fixtures::tone_mp3};
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};
#[cfg(feature = "apple")]
use num_traits::AsPrimitive;

mod consts {
    pub(super) const WAV_CHANNELS: u16 = 2;
    pub(super) const WAV_FRAMES: usize = WAV_SAMPLE_RATE as usize * 2;
    pub(super) const WAV_SAMPLE_RATE: u32 = 44_100;
}

/// Backend selector for cross-decoder comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Default,
    #[cfg(feature = "apple")]
    Apple,
}

impl Backend {
    fn make_decoder(self, bytes: Vec<u8>, info: &MediaInfo) -> Box<dyn Decoder> {
        let source = Cursor::new(bytes);
        let config = DecoderConfig::<kithara_resampler::NoResamplerBackend, TestPools>::builder()
            .pools(pools())
            .backend(self.to_choice())
            .build();
        DecoderFactory::create_from_media_info(source, info, config)
            .unwrap_or_else(|e| panic!("{self:?} decoder should create: {e}"))
    }

    fn make_mp3(self, tone_mp3: &[u8]) -> Box<dyn Decoder> {
        let info = MediaInfo::builder()
            .maybe_codec(Some(AudioCodec::Mp3))
            .maybe_container(Some(ContainerFormat::MpegAudio))
            .build();
        self.make_decoder(tone_mp3.to_vec(), &info)
    }

    fn make_wav(self, bytes: &[u8]) -> Box<dyn Decoder> {
        let info = MediaInfo::builder()
            .maybe_codec(Some(AudioCodec::Pcm))
            .maybe_container(Some(ContainerFormat::Wav))
            .build();
        self.make_decoder(bytes.to_vec(), &info)
    }

    fn to_choice(self) -> kithara_decode::DecoderBackend {
        use kithara_decode::DecoderBackend;
        match self {
            Self::Default => DecoderBackend::default(),
            #[cfg(feature = "apple")]
            Self::Apple => DecoderBackend::Apple,
        }
    }
}

/// Drain the decoder and return concatenated f32 PCM samples.
fn drain_all(decoder: &mut dyn Decoder) -> Vec<f32> {
    let mut all = Vec::new();
    loop {
        match decoder.next_chunk().expect("decode should succeed") {
            DecoderChunkOutcome::Chunk(chunk) => {
                all.extend_from_slice(&chunk.samples);
            }
            DecoderChunkOutcome::Pending(_) => continue,
            DecoderChunkOutcome::Eof => break,
        }
    }
    all
}

/// L2 norm of a PCM slice.
#[cfg(feature = "apple")]
fn l2_norm(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|s| f64::from(*s * *s))
        .sum::<f64>()
        .sqrt()
}

#[kithara::test]
#[case::default(Backend::Default)]
#[cfg_attr(feature = "apple", case::apple(platform = apple, Backend::Apple))]
fn spec_after_create_is_consistent_across_backends(
    #[case] backend: Backend,
    tone_mp3: &'static [u8],
) {
    let reference = Backend::Default.make_mp3(tone_mp3).spec();
    let spec = backend.make_mp3(tone_mp3).spec();
    assert_eq!(
        spec,
        reference,
        "spec mismatch between {:?} and {backend:?}: {reference:?} vs {spec:?}",
        Backend::Default
    );
}

#[kithara::test]
#[case::default(Backend::Default)]
#[cfg_attr(feature = "apple", case::apple(platform = apple, Backend::Apple))]
fn duration_after_create_is_consistent_across_backends(
    #[case] backend: Backend,
    tone_mp3: &'static [u8],
) {
    let Some(kithara_decode::DecoderDuration::RawLength(first)) =
        Backend::Default.make_mp3(tone_mp3).duration()
    else {
        panic!("Symphonia must report a raw PCM length");
    };
    let Some(kithara_decode::DecoderDuration::RawLength(other)) =
        backend.make_mp3(tone_mp3).duration()
    else {
        panic!("backend {backend:?} must report a raw PCM length for a complete MP3 file")
    };
    assert!(
        other.as_secs_f64() > 1.0,
        "duration from {backend:?} is suspiciously short: {other:?}"
    );
    let diff = (first.as_secs_f64() - other.as_secs_f64()).abs();
    let tol = first.as_secs_f64() * 0.05;
    assert!(
        diff <= tol,
        "duration diverges between Symphonia ({first:?}) and {backend:?} ({other:?}); \
         diff={diff:.3}s, tol={tol:.3}s"
    );
}

#[kithara::test(timeout(Medium))]
#[case::default(Backend::Default)]
#[cfg_attr(feature = "apple", case::apple(platform = apple, Backend::Apple))]
fn total_frames_are_consistent_across_backends(#[case] backend: Backend, tone_mp3: &'static [u8]) {
    let frames = |backend: Backend| {
        let mut dec = backend.make_mp3(tone_mp3);
        let samples = drain_all(&mut *dec);
        samples.len() / dec.spec().channels.max(1) as usize
    };
    let first = frames(Backend::Default);
    let other = frames(backend);
    let tol = first.max(other) / 100;
    let diff = first.abs_diff(other);
    assert!(
        diff <= tol,
        "frame count mismatch: {:?}={first}, {backend:?}={other}, diff={diff} > tol={tol}",
        Backend::Default
    );
}

#[kithara::test]
#[case::default(Backend::Default)]
#[cfg_attr(feature = "apple", case::apple(platform = apple, Backend::Apple))]
fn seek_then_first_chunk_timestamp_is_after_target(
    #[case] backend: Backend,
    tone_mp3: &'static [u8],
) {
    const TARGET: Duration = Duration::from_millis(500);
    let tol = Duration::from_millis(200);

    let mut dec = backend.make_mp3(tone_mp3);
    dec.seek(TARGET).expect("seek should succeed");
    let outcome = dec.next_chunk().expect("next_chunk after seek");
    let chunk = AudioChunk::try_from(outcome).expect("at least one chunk after a 0.5s seek");

    let ts = chunk.meta.timestamp;
    assert!(
        ts + tol >= TARGET,
        "{backend:?}: first chunk after seek at {ts:?} is earlier than target {TARGET:?} - tol {tol:?}"
    );
}

#[kithara::test(timeout(Long))]
#[case::default(Backend::Default)]
#[cfg_attr(feature = "apple", case::apple(platform = apple, Backend::Apple))]
fn end_of_stream_returns_none_repeatedly(#[case] backend: Backend, tone_mp3: &'static [u8]) {
    let mut dec = backend.make_mp3(tone_mp3);
    while matches!(
        dec.next_chunk().expect("decode before EOF should succeed"),
        DecoderChunkOutcome::Chunk(_)
    ) {}

    for _ in 0..3 {
        let next = dec.next_chunk().expect("decode after EOF");
        assert!(
            matches!(next, DecoderChunkOutcome::Eof),
            "backend {backend:?} must keep returning Eof at end-of-stream"
        );
    }
}

#[kithara::test]
#[case::default(Backend::Default)]
#[cfg_attr(feature = "apple", case::apple(platform = apple, Backend::Apple))]
fn wav_pcm_round_trip_matches_signal_across_backends(
    #[case] backend: Backend,
    protocol_wav: &'static [u8],
) {
    let mut dec = backend.make_wav(protocol_wav);

    let spec = dec.spec();
    assert_eq!(
        spec.sample_rate.get(),
        consts::WAV_SAMPLE_RATE,
        "{backend:?}: sample rate mismatch"
    );
    assert_eq!(
        spec.channels,
        consts::WAV_CHANNELS,
        "{backend:?}: channel count mismatch"
    );

    let samples = drain_all(&mut *dec);
    let channels = usize::from(spec.channels).max(1);
    let frames = samples.len() / channels;

    let tol = consts::WAV_FRAMES / 200;
    let diff = consts::WAV_FRAMES.abs_diff(frames);
    let expected = consts::WAV_FRAMES;
    assert!(
        diff <= tol,
        "{backend:?}: decoded {frames} frames, expected ~{expected} (tol {tol})"
    );

    let peak = samples.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
    assert!(
        peak > 0.1,
        "{backend:?}: decoded PCM peak {peak} is too quiet — signal likely dropped"
    );
}

/// Standalone (non-fMP4) container codecs the Apple `AudioFileServices`
/// path must decode. Each case yields the encoded fixture bytes plus the
/// `MediaInfo` the production stream layer detects for it.
#[cfg(feature = "apple")]
#[derive(Clone, Copy, Debug)]
enum StandaloneCase {
    AlacM4a,
    NativeFlac,
}

#[cfg(feature = "apple")]
impl StandaloneCase {
    fn fixture(self, standalone_audio: [&'static [u8]; 2]) -> (Vec<u8>, MediaInfo) {
        match self {
            Self::AlacM4a => (
                standalone_audio[0].to_vec(),
                MediaInfo::builder()
                    .maybe_codec(Some(AudioCodec::Alac))
                    .maybe_container(Some(ContainerFormat::Mp4))
                    .build(),
            ),
            Self::NativeFlac => (
                standalone_audio[1].to_vec(),
                MediaInfo::builder()
                    .maybe_codec(Some(AudioCodec::Flac))
                    .maybe_container(Some(ContainerFormat::Flac))
                    .build(),
            ),
        }
    }
}

/// Apple's standalone `AudioFileServices` path decodes every non-fMP4
/// container codec the production stream layer routes to it. `NativeFlac`
/// is the regression contract: zvuk `streamfl` tracks are raw `fLaC`
/// bitstreams that the device must decode through the Apple backend (the
/// only decoder shipped in the iOS build — no Symphonia fallback).
#[cfg(feature = "apple")]
#[kithara::test(platform = apple)]
#[case::alac(StandaloneCase::AlacM4a)]
#[case::native_flac(StandaloneCase::NativeFlac)]
fn apple_decodes_standalone(#[case] case: StandaloneCase, standalone_audio: [&'static [u8]; 2]) {
    let (bytes, info) = case.fixture(standalone_audio);
    let mut decoder = Backend::Apple.make_decoder(bytes, &info);

    let spec = decoder.spec();
    assert!(
        spec.sample_rate.get() >= 8000,
        "{case:?} sample rate sanity"
    );
    assert!(spec.channels >= 1, "{case:?} channel count sanity");

    let samples = drain_all(&mut *decoder);
    let frames = samples.len() / usize::from(spec.channels.max(1));
    let expected: usize = spec.sample_rate.get().as_(); // 1-second fixtures
    let tol = expected / 10;
    assert!(
        frames.abs_diff(expected) <= tol,
        "Apple {case:?} produced {frames} frames, expected ~{expected} (tol {tol})"
    );

    // The FLAC fixture is a non-silent sawtooth, so audible PCM proves the
    // `dfLa` magic cookie was decoded for real — not silence/garbage that a
    // frame count alone would let through. (The ALAC fixture is silence.)
    if matches!(case, StandaloneCase::NativeFlac) {
        let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1, "Apple FLAC decoded near-silence — peak {peak}");
    }
}

#[cfg(feature = "apple")]
#[kithara::test(platform = apple, timeout(Medium))]
fn full_decode_l2_norm_matches_within_tolerance(tone_mp3: &'static [u8]) {
    const TOL_REL: f64 = 0.02;

    let mut sym = Backend::Default.make_mp3(tone_mp3);
    let mut apl = Backend::Apple.make_mp3(tone_mp3);

    let sym_samples = drain_all(&mut *sym);
    let apl_samples = drain_all(&mut *apl);

    let sym_l2 = l2_norm(&sym_samples);
    let apl_l2 = l2_norm(&apl_samples);

    let reference = sym_l2.max(apl_l2);
    assert!(reference > 0.0, "decoded PCM must have non-zero energy");

    let rel = (sym_l2 - apl_l2).abs() / reference;
    assert!(
        rel <= TOL_REL,
        "L2-norm diverges between Symphonia ({:.3}) and Apple ({:.3}); rel={:.4} > tol={}",
        sym_l2,
        apl_l2,
        rel,
        TOL_REL
    );
}

#[kithara::fixture]
fn protocol_wav() -> &'static [u8] {
    sine_wav_a440_full_scale_2s().bytes()
}

#[cfg(feature = "apple")]
#[kithara::fixture]
fn standalone_audio() -> [&'static [u8]; 2] {
    let flac = flac_unknown_length_saw_1s().bytes();
    assert_eq!(&flac[..4], b"fLaC", "fixture must be native FLAC");
    [alac_silence_1s().bytes(), flac]
}
