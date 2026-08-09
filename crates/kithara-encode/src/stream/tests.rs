use super::{StreamBackend, StreamEncoder};
use crate::{EncodeError, EncodedAccessUnit, test_pcm::TestPcm};

struct Consts;

impl Consts {
    const BIT_RATE: u64 = 128_000;
    const CHANNELS: u16 = 2;
    const FRAMES: usize = 4_096;
    const SAMPLE_RATE: u32 = 48_000;
}

fn encode_in_chunks(
    backend: StreamBackend,
    samples: &[f32],
    chunk_frames: usize,
    timescale: u32,
) -> Vec<EncodedAccessUnit> {
    let mut encoder = StreamEncoder::new(
        backend,
        Consts::SAMPLE_RATE,
        Consts::CHANNELS,
        Consts::BIT_RATE,
        timescale,
    )
    .expect("stream encoder");
    let mut units = Vec::new();
    for chunk in samples.chunks(chunk_frames * usize::from(Consts::CHANNELS)) {
        units.extend(encoder.push(chunk).expect("push"));
    }
    units.extend(encoder.finish().expect("finish"));
    units
}

/// The same audio pushed in any chunking yields the same access units.
fn chunking_does_not_change_the_encoded_stream(backend: StreamBackend) {
    let samples =
        TestPcm::sawtooth(Consts::FRAMES, Consts::SAMPLE_RATE, Consts::CHANNELS).samples_f32();

    let whole = encode_in_chunks(backend, &samples, Consts::FRAMES, Consts::SAMPLE_RATE);
    let framed = encode_in_chunks(
        backend,
        &samples,
        StreamEncoder::FRAME_SAMPLES,
        Consts::SAMPLE_RATE,
    );
    let ragged = encode_in_chunks(backend, &samples, 333, Consts::SAMPLE_RATE);

    assert!(!whole.is_empty(), "encoder produced no access units");
    assert_eq!(whole, framed);
    assert_eq!(whole, ragged);
}

/// Timestamps start at zero and every access unit carries one frame, on a
/// timescale that divides the sample rate and on one that does not. The stream
/// covers the pushed audio plus the backend's priming, which is what proves the
/// flush hands the tail back.
fn timestamps_start_at_zero_and_advance_by_one_frame(
    backend: StreamBackend,
    priming_frames: usize,
) {
    let frame_samples = u64::try_from(StreamEncoder::FRAME_SAMPLES).expect("frame size fits u64");
    let encoded_frames =
        u64::try_from(Consts::FRAMES + priming_frames).expect("frame count fits u64");
    let samples =
        TestPcm::sawtooth(Consts::FRAMES, Consts::SAMPLE_RATE, Consts::CHANNELS).samples_f32();

    for timescale in [Consts::SAMPLE_RATE, 90_000] {
        let rescale = |frames: u64| frames * u64::from(timescale) / u64::from(Consts::SAMPLE_RATE);
        let units = encode_in_chunks(backend, &samples, StreamEncoder::FRAME_SAMPLES, timescale);

        let mut expected_pts = 0;
        for unit in &units {
            assert!(!unit.bytes.is_empty(), "access unit payload is empty");
            assert!(unit.is_sync, "every AAC-LC access unit is a sync point");
            assert_eq!(unit.pts, expected_pts, "timescale {timescale}");
            assert_eq!(unit.dts, unit.pts, "AAC access units are not reordered");
            assert_eq!(
                u64::from(unit.duration),
                rescale(frame_samples),
                "timescale {timescale}"
            );
            expected_pts += u64::from(unit.duration);
        }

        assert_eq!(
            expected_pts,
            rescale(encoded_frames),
            "the stream covers the pushed audio plus {priming_frames} frames of priming"
        );
    }
}

/// Access-unit boundaries are what gets rescaled, so durations tile the pts
/// timeline even when the timescale ratio is fractional.
fn a_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline(
    backend: StreamBackend,
    priming_frames: usize,
) {
    const SOURCE_RATE: u32 = 44_100;
    const TIMESCALE: u32 = 90_000;

    let samples = TestPcm::sawtooth(Consts::FRAMES, SOURCE_RATE, Consts::CHANNELS).samples_f32();
    let mut encoder = StreamEncoder::new(
        backend,
        SOURCE_RATE,
        Consts::CHANNELS,
        Consts::BIT_RATE,
        TIMESCALE,
    )
    .expect("stream encoder");
    let mut units = encoder.push(&samples).expect("push");
    units.extend(encoder.finish().expect("finish"));

    let mut expected_pts = 0;
    for unit in &units {
        assert_eq!(
            unit.pts, expected_pts,
            "durations must tile the pts timeline"
        );
        expected_pts += u64::from(unit.duration);
    }

    let frames = u64::try_from(Consts::FRAMES + priming_frames).expect("fits u64");
    let ticks = frames * u64::from(TIMESCALE);
    assert_eq!(
        expected_pts,
        (ticks + u64::from(SOURCE_RATE) / 2) / u64::from(SOURCE_RATE),
        "the stream ends on the rounded rescale of the encoded frames"
    );
}

fn new_rejects_a_channel_count_the_encoder_cannot_carry(backend: StreamBackend) {
    assert!(
        StreamEncoder::new(
            backend,
            Consts::SAMPLE_RATE,
            9,
            Consts::BIT_RATE,
            Consts::SAMPLE_RATE
        )
        .is_err(),
        "AAC-LC carries no 9-channel layout"
    );
}

fn push_rejects_a_partial_frame(backend: StreamBackend) {
    let mut encoder = StreamEncoder::new(
        backend,
        Consts::SAMPLE_RATE,
        Consts::CHANNELS,
        Consts::BIT_RATE,
        Consts::SAMPLE_RATE,
    )
    .expect("stream encoder");

    let error = encoder.push(&[0.0, 0.0, 0.0]).expect_err("partial frame");

    assert!(matches!(error, EncodeError::InvalidInput(_)), "{error}");
}

fn new_rejects_audio_no_backend_can_carry(backend: StreamBackend) {
    for (sample_rate, channels, timescale) in [
        (0, Consts::CHANNELS, Consts::SAMPLE_RATE),
        (Consts::SAMPLE_RATE, 0, Consts::SAMPLE_RATE),
        (Consts::SAMPLE_RATE, Consts::CHANNELS, 0),
    ] {
        let error = StreamEncoder::new(backend, sample_rate, channels, Consts::BIT_RATE, timescale)
            .map(|_| ())
            .expect_err("zero is not audio");

        assert!(matches!(error, EncodeError::InvalidInput(_)), "{error}");
    }
}

#[cfg(feature = "ffmpeg")]
mod ffmpeg {
    use super::{StreamBackend, StreamEncoder};

    /// `FFmpeg`'s AAC encoder holds one frame of priming and flushes it.
    const PRIMING_FRAMES: usize = StreamEncoder::FRAME_SAMPLES;
    const BACKEND: StreamBackend = StreamBackend::Ffmpeg;

    #[test]
    fn chunking_does_not_change_the_encoded_stream() {
        super::chunking_does_not_change_the_encoded_stream(BACKEND);
    }

    #[test]
    fn timestamps_start_at_zero_and_advance_by_one_frame() {
        super::timestamps_start_at_zero_and_advance_by_one_frame(BACKEND, PRIMING_FRAMES);
    }

    #[test]
    fn a_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline() {
        super::a_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline(
            BACKEND,
            PRIMING_FRAMES,
        );
    }

    #[test]
    fn new_rejects_a_channel_count_the_encoder_cannot_carry() {
        super::new_rejects_a_channel_count_the_encoder_cannot_carry(BACKEND);
    }

    #[test]
    fn push_rejects_a_partial_frame() {
        super::push_rejects_a_partial_frame(BACKEND);
    }

    #[test]
    fn new_rejects_audio_no_backend_can_carry() {
        super::new_rejects_audio_no_backend_can_carry(BACKEND);
    }
}

#[cfg(feature = "fdk-aac")]
mod fdk {
    use super::{StreamBackend, StreamEncoder};

    /// libfdk reports a 2048-sample AAC-LC delay, which the flush pads out to
    /// two access units.
    const PRIMING_FRAMES: usize = 2 * StreamEncoder::FRAME_SAMPLES;
    const BACKEND: StreamBackend = StreamBackend::Fdk;

    #[test]
    fn chunking_does_not_change_the_encoded_stream() {
        super::chunking_does_not_change_the_encoded_stream(BACKEND);
    }

    #[test]
    fn timestamps_start_at_zero_and_advance_by_one_frame() {
        super::timestamps_start_at_zero_and_advance_by_one_frame(BACKEND, PRIMING_FRAMES);
    }

    #[test]
    fn a_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline() {
        super::a_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline(
            BACKEND,
            PRIMING_FRAMES,
        );
    }

    #[test]
    fn new_rejects_a_channel_count_the_encoder_cannot_carry() {
        super::new_rejects_a_channel_count_the_encoder_cannot_carry(BACKEND);
    }

    #[test]
    fn push_rejects_a_partial_frame() {
        super::push_rejects_a_partial_frame(BACKEND);
    }

    #[test]
    fn new_rejects_audio_no_backend_can_carry() {
        super::new_rejects_audio_no_backend_can_carry(BACKEND);
    }
}
