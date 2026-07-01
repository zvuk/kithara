use std::io::Cursor;

use kithara_decode::{Decoder, DecoderConfig, DecoderFactory, PcmChunk};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};
use kithara_integration_tests::{create_test_wav, decode_ext::DecoderChunkOutcomeTestExt};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use kithara_integration_tests::{encode_test_pcm::SawtoothPcmFixture, ensure_silence_1s_alac_m4a};
use kithara_platform::time::Duration;
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_utils::kithara;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use num_traits::AsPrimitive;

struct Consts;

impl Consts {
    const WAV_CHANNELS: u16 = 2;
    const WAV_FRAMES: usize = Self::WAV_SAMPLE_RATE as usize * 2;
    const WAV_SAMPLE_RATE: u32 = 44_100;
}

/// Backend selector for cross-decoder comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Symphonia,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Apple,
}

impl Backend {
    fn make_decoder(self, bytes: Vec<u8>, info: &MediaInfo) -> Box<dyn Decoder> {
        let source = Cursor::new(bytes);
        let config = DecoderConfig::builder().backend(self.to_choice()).build();
        DecoderFactory::create_from_media_info(source, info, config)
            .unwrap_or_else(|e| panic!("{self:?} decoder should create: {e}"))
    }

    fn make_mp3(self) -> Box<dyn Decoder> {
        const TEST_MP3_BYTES: &[u8] =
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../assets/test.mp3"));
        let info = MediaInfo::new(Some(AudioCodec::Mp3), Some(ContainerFormat::MpegAudio));
        self.make_decoder(TEST_MP3_BYTES.to_vec(), &info)
    }

    fn make_wav(self) -> Box<dyn Decoder> {
        let bytes = create_test_wav(
            Consts::WAV_FRAMES,
            Consts::WAV_SAMPLE_RATE,
            Consts::WAV_CHANNELS,
        );
        let info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
        self.make_decoder(bytes, &info)
    }

    fn to_choice(self) -> kithara_decode::DecoderBackend {
        use kithara_decode::DecoderBackend;
        match self {
            Self::Symphonia => DecoderBackend::Symphonia,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Self::Apple => DecoderBackend::Apple,
        }
    }
}

fn available_backends() -> Vec<Backend> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        vec![Backend::Symphonia, Backend::Apple]
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        vec![Backend::Symphonia]
    }
}

/// Drain the decoder and return concatenated f32 PCM samples.
fn drain_all(decoder: &mut dyn Decoder) -> Vec<f32> {
    let mut all = Vec::new();
    loop {
        match decoder.next_chunk().expect("decode should succeed") {
            kithara_decode::DecoderChunkOutcome::Chunk(chunk) => {
                all.extend_from_slice(&chunk.samples);
            }
            kithara_decode::DecoderChunkOutcome::Pending(_) => continue,
            kithara_decode::DecoderChunkOutcome::Eof => break,
        }
    }
    all
}

/// L2 norm of a PCM slice.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn l2_norm(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|s| f64::from(*s * *s))
        .sum::<f64>()
        .sqrt()
}

#[kithara::test]
fn spec_after_create_is_consistent_across_backends() {
    let specs: Vec<_> = available_backends()
        .into_iter()
        .map(|b| (b, b.make_mp3().spec()))
        .collect();

    let first = specs[0].1;
    for (backend, spec) in &specs[1..] {
        assert_eq!(
            *spec, first,
            "spec mismatch between {:?} and {:?}: {:?} vs {:?}",
            specs[0].0, backend, first, spec
        );
    }
}

#[kithara::test]
fn duration_after_create_is_consistent_across_backends() {
    let durations: Vec<_> = available_backends()
        .into_iter()
        .map(|b| (b, b.make_mp3().duration()))
        .collect();

    for (backend, duration) in &durations {
        let d = duration.unwrap_or_else(|| {
            panic!("backend {backend:?} must report duration for a complete MP3 file")
        });
        assert!(
            d.as_secs_f64() > 1.0,
            "duration from {backend:?} is suspiciously short: {d:?}"
        );
    }

    let first = durations[0].1.expect("symphonia reports duration");
    for (backend, duration) in &durations[1..] {
        let other = duration.expect("backend reports duration");
        let diff = (first.as_secs_f64() - other.as_secs_f64()).abs();
        let tol = first.as_secs_f64() * 0.05;
        assert!(
            diff <= tol,
            "duration diverges between Symphonia ({:?}) and {:?} ({:?}); diff={:.3}s, tol={:.3}s",
            first,
            backend,
            other,
            diff,
            tol
        );
    }
}

#[kithara::test]
fn total_frames_are_consistent_across_backends() {
    let frame_counts: Vec<_> = available_backends()
        .into_iter()
        .map(|b| {
            let mut dec = b.make_mp3();
            let samples = drain_all(&mut *dec);
            let channels = dec.spec().channels.max(1) as usize;
            let frames = samples.len() / channels;
            (b, frames)
        })
        .collect();

    let first = frame_counts[0].1;
    for (backend, frames) in &frame_counts[1..] {
        let tol = first.max(*frames) / 100;
        let diff = first.abs_diff(*frames);
        assert!(
            diff <= tol,
            "frame count mismatch: {:?}={}, {:?}={}, diff={} > tol={}",
            frame_counts[0].0,
            first,
            backend,
            frames,
            diff,
            tol
        );
    }
}

#[kithara::test]
fn seek_then_first_chunk_timestamp_is_after_target() {
    const TARGET: Duration = Duration::from_millis(500);
    let tol = Duration::from_millis(200);

    for backend in available_backends() {
        let mut dec = backend.make_mp3();
        dec.seek(TARGET).expect("seek should succeed");
        let outcome = dec.next_chunk().expect("next_chunk after seek");
        let chunk = PcmChunk::try_from(outcome).expect("at least one chunk after a 0.5s seek");

        let ts = chunk.meta.timestamp;
        assert!(
            ts + tol >= TARGET,
            "{:?}: first chunk after seek at {:?} is earlier than target {:?} - tol {:?}",
            backend,
            ts,
            TARGET,
            tol
        );
    }
}

#[kithara::test]
fn end_of_stream_returns_none_repeatedly() {
    for backend in available_backends() {
        let mut dec = backend.make_mp3();
        while dec
            .next_chunk()
            .expect("decode before EOF should succeed")
            .is_chunk()
        {}

        for _ in 0..3 {
            let next = dec.next_chunk().expect("decode after EOF");
            assert!(
                next.is_eof(),
                "backend {backend:?} must keep returning Eof at end-of-stream"
            );
        }
    }
}

#[kithara::test]
fn wav_pcm_round_trip_matches_signal_across_backends() {
    for backend in available_backends() {
        let mut dec = backend.make_wav();

        let spec = dec.spec();
        assert_eq!(
            spec.sample_rate.get(),
            Consts::WAV_SAMPLE_RATE,
            "{backend:?}: sample rate mismatch"
        );
        assert_eq!(
            spec.channels,
            Consts::WAV_CHANNELS,
            "{backend:?}: channel count mismatch"
        );

        let samples = drain_all(&mut *dec);
        let channels = usize::from(spec.channels).max(1);
        let frames = samples.len() / channels;

        let tol = Consts::WAV_FRAMES / 200;
        let diff = Consts::WAV_FRAMES.abs_diff(frames);
        let expected = Consts::WAV_FRAMES;
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
}

/// Standalone (non-fMP4) container codecs the Apple `AudioFileServices`
/// path must decode. Each case yields the encoded fixture bytes plus the
/// `MediaInfo` the production stream layer detects for it.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[derive(Clone, Copy, Debug)]
enum StandaloneCase {
    AlacM4a,
    NativeFlac,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl StandaloneCase {
    fn fixture(self) -> (Vec<u8>, MediaInfo) {
        match self {
            Self::AlacM4a => {
                let path = ensure_silence_1s_alac_m4a();
                let bytes = std::fs::read(&path).expect("read ALAC fixture");
                (
                    bytes,
                    MediaInfo::new(Some(AudioCodec::Alac), Some(ContainerFormat::Mp4)),
                )
            }
            Self::NativeFlac => (
                synth_native_flac_1s(),
                MediaInfo::new(Some(AudioCodec::Flac), Some(ContainerFormat::Flac)),
            ),
        }
    }
}

/// Encode 1 second of stereo 44.1 kHz sawtooth PCM into a native `fLaC`
/// bitstream via `kithara-encode`. Mirrors the raw FLAC body the zvuk
/// `streamfl` endpoint serves (Content-Type `audio/flac`, magic `fLaC`)
/// that the device misdecodes — the pinned contract for that regression.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn synth_native_flac_1s() -> Vec<u8> {
    let pcm = SawtoothPcmFixture::new(
        Consts::WAV_FRAMES / 2,
        Consts::WAV_SAMPLE_RATE,
        Consts::WAV_CHANNELS,
    );
    let encoded = EncoderFactory::encode_bytes(BytesEncodeRequest {
        pcm: &pcm,
        target: BytesEncodeTarget::Flac,
        bit_rate: None,
    })
    .expect("encode synthetic native FLAC bytes");
    assert_eq!(
        &encoded.bytes[..4],
        b"fLaC",
        "kithara-encode must emit a native FLAC bitstream"
    );
    encoded.bytes
}

/// Apple's standalone `AudioFileServices` path decodes every non-fMP4
/// container codec the production stream layer routes to it. `NativeFlac`
/// is the regression contract: zvuk `streamfl` tracks are raw `fLaC`
/// bitstreams that the device must decode through the Apple backend (the
/// only decoder shipped in the iOS build — no Symphonia fallback).
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[kithara::test]
#[case::alac(StandaloneCase::AlacM4a)]
#[case::native_flac(StandaloneCase::NativeFlac)]
fn apple_decodes_standalone(#[case] case: StandaloneCase) {
    let (bytes, info) = case.fixture();
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

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[kithara::test]
fn full_decode_l2_norm_matches_within_tolerance() {
    const TOL_REL: f64 = 0.02;

    let mut sym = Backend::Symphonia.make_mp3();
    let mut apl = Backend::Apple.make_mp3();

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
