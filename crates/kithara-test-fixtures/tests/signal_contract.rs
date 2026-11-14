#![cfg(not(target_arch = "wasm32"))]

//! The signal primitives read against the prepared inputs the build wrote: a
//! detector is only as good as what it says about a stored signal.

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::{
    assets,
    fixtures::{
        ascending_pcm, ascending_wrap_pcm, descending_pcm, descending_wrap_pcm,
        direction_channel_less, direction_step, phase_endpoints, provenance_silence,
    },
    integration_fixtures::{cochlea_pulses, cochlea_silence, constant_half, encoder_saw_aac},
    signal::{
        FrameClass, Pcm, Replay, SAW_PERIOD, SignalDirection, ascending_phase_replays,
        classify_windows, detect_direction, phase::units,
    },
};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

mod consts {
    pub(super) const WINDOW: usize = 64;
}

#[kithara::test(native)]
fn prepared_constant_pcm_wav_matches_the_previous_deck_writer(constant_half: &'static [u8]) {
    let mut expected = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x03\0".to_vec();
    expected.extend_from_slice(&2_u16.to_le_bytes());
    expected.extend_from_slice(&44_100_u32.to_le_bytes());
    expected.extend_from_slice(&352_800_u32.to_le_bytes());
    expected.extend_from_slice(&8_u16.to_le_bytes());
    expected.extend_from_slice(&32_u16.to_le_bytes());
    expected.extend_from_slice(b"data\0\0\0\0");
    for sample in constant_half[..44_100 * size_of::<f32>()]
        .as_chunks::<4>()
        .0
    {
        expected.extend_from_slice(sample);
        expected.extend_from_slice(sample);
    }
    let data_size = u32::try_from(expected.len() - 44).expect("one-second WAV data fits u32");
    expected[4..8].copy_from_slice(&(data_size + 36).to_le_bytes());
    expected[40..44].copy_from_slice(&data_size.to_le_bytes());
    assert_eq!(expected.len(), 352_844);

    let asset = assets::constant_half_wav_default();
    let path = asset.path().expect("prepared float WAV is stored on disk");
    assert_eq!(asset.bytes(), expected);
    assert_eq!(
        std::fs::read(path).expect("read prepared float WAV"),
        expected
    );
}

#[kithara::test(native)]
#[case::cd_120(44_100, 120.0)]
#[case::render_120(48_000, 120.0)]
#[case::cd_127(44_100, 127.0)]
#[case::render_127(48_000, 127.0)]
#[case::cd_126(44_100, 126.0)]
#[case::render_126(48_000, 126.0)]
#[case::cd_128(44_100, 128.0)]
#[case::render_128(48_000, 128.0)]
#[case::cd_126_49(44_100, 126.49)]
#[case::render_126_49(48_000, 126.49)]
#[case::cd_127_51(44_100, 127.51)]
#[case::render_127_51(48_000, 127.51)]
#[case::half(48_000, 63.5)]
fn prepared_pulses_preserve_the_original_sample_bits(#[case] sample_rate: u32, #[case] bpm: f64) {
    let prepared = cochlea_pulses(sample_rate, bpm);
    let beat_frames = f64::from(sample_rate) * 60.0 / bpm;
    let leading_frames = usize::try_from(sample_rate).expect("sample rate fits usize") / 4;
    let pulse_frames = usize::try_from(sample_rate).expect("sample rate fits usize") / 50;
    let mut original = vec![
        0.0_f32;
        leading_frames
            + (beat_frames * 6.0)
                .ceil()
                .to_usize()
                .expect("finite pulse extent")
    ];
    for beat in 0..6 {
        let start = leading_frames
            + (f64::from(beat) * beat_frames)
                .round()
                .to_usize()
                .expect("finite beat offset");
        let amplitude = if beat % 4 == 0 { 0.9 } else { 0.7 };
        for offset in 0..pulse_frames {
            let phase =
                std::f32::consts::TAU * 1_000.0 * offset.to_f32().expect("pulse offset fits f32")
                    / sample_rate.to_f32().expect("rate fits f32");
            original[start + offset] = amplitude
                * phase.cos()
                * (1.0
                    - offset.to_f32().expect("pulse offset fits f32")
                        / pulse_frames.to_f32().expect("pulse extent fits f32"));
        }
    }
    assert_eq!(prepared.len(), original.len());
    assert!(
        prepared
            .iter()
            .zip(&original)
            .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
    );
}

#[kithara::test(native)]
fn prepared_oracle_silence_retains_its_full_extent() {
    let silence = cochlea_silence();
    assert_eq!(silence.len(), 144_000);
    assert!(silence.iter().all(|sample| sample.to_bits() == 0));
}

#[kithara::test(native, timeout(Short))]
fn prepared_bytes_preserve_pcm_and_reject_incomplete_frames(encoder_saw_aac: Pcm) {
    let bytes = Vec::from(encoder_saw_aac);
    let pcm = Pcm::from((48_000, 2, bytes.clone()));
    assert_eq!(pcm.sample_rate(), 48_000);
    assert_eq!(pcm.channels(), 2);
    assert_eq!(Vec::from(pcm), bytes);
    for (rate, channels, trim) in [(0, 2, 0), (48_000, 0, 0), (48_000, 2, 1)] {
        let invalid = bytes[..bytes.len() - trim].to_vec();
        assert!(std::panic::catch_unwind(|| Pcm::from((rate, channels, invalid))).is_err());
    }
}

#[kithara::test(native)]
fn units_round_trip_the_i16_sawtooth_range(phase_endpoints: Vec<f32>) {
    assert_eq!(units(phase_endpoints[0]), 0);
    assert_eq!(units(phase_endpoints[1]), 1);
    assert_eq!(units(phase_endpoints[2]), 32_768);
    assert_eq!(units(phase_endpoints[3]), SAW_PERIOD - 1);
}

#[kithara::test(native)]
fn one_ascending_step_per_frame_reads_as_ascending(direction_step: Vec<f32>) {
    assert_eq!(
        detect_direction(&direction_step, 2),
        SignalDirection::Ascending
    );
}

#[kithara::test(native)]
fn a_channel_less_buffer_has_no_direction(direction_channel_less: Vec<f32>) {
    assert_eq!(
        detect_direction(&direction_channel_less, 0),
        SignalDirection::Unknown
    );
}

#[kithara::test(native)]
fn classify_windows_labels_pure_signals_including_wrap(
    ascending_wrap_pcm: Vec<f32>,
    descending_wrap_pcm: Vec<f32>,
    provenance_silence: Vec<f32>,
) {
    let ascending = &ascending_wrap_pcm[..consts::WINDOW];
    assert_eq!(
        classify_windows(ascending, consts::WINDOW, 0.5),
        vec![FrameClass::Ascending]
    );

    let descending = descending_wrap_pcm;
    assert_eq!(
        classify_windows(&descending, consts::WINDOW, 0.5),
        vec![FrameClass::Descending]
    );

    let silence = provenance_silence;
    assert_eq!(
        classify_windows(&silence, consts::WINDOW, 0.5),
        vec![FrameClass::Silence]
    );
}

/// A splice is a splice wherever it falls. The frame it lands on is set by
/// whatever the renderer had committed when the flush arrived, so a reader
/// that saw only the steps inside a window would report the same stream as
/// continuous in one run and broken in the next.
#[kithara::test(native)]
fn a_splice_on_a_window_boundary_still_breaks_the_class(ascending_pcm: Vec<f32>) {
    const JUMP: usize = SAW_PERIOD / 2;

    let mut left = ascending_pcm[..consts::WINDOW * 2].to_vec();
    left[consts::WINDOW..].copy_from_slice(&ascending_pcm[JUMP..JUMP + consts::WINDOW]);

    assert_eq!(
        classify_windows(&left, consts::WINDOW, 0.5),
        vec![FrameClass::Ascending, FrameClass::Unknown]
    );
}

#[kithara::test(native)]
fn ascending_phase_replays_accepts_pure_ascending_run_including_wrap(ascending_wrap_pcm: Vec<f32>) {
    let left = ascending_wrap_pcm;

    assert!(ascending_phase_replays(&left, 0, left.len(), 3).is_empty());
}

#[kithara::test(native)]
fn ascending_phase_replays_reports_spliced_replay_from_start(ascending_pcm: Vec<f32>) {
    let splice_start = 200_000;
    let replay_len = 1_000;
    let total_len = splice_start + replay_len + 2_000;
    let mut left = ascending_pcm[..total_len].to_vec();
    let replay = &ascending_pcm[..replay_len];
    left[splice_start..splice_start + replay_len].copy_from_slice(replay);

    let replays = ascending_phase_replays(&left, 0, left.len(), 3);

    assert_eq!(replays.len(), 1);
    assert_replay(
        replays[0],
        Replay {
            start_frame: splice_start,
            len: replay_len,
            start_phase: 0,
        },
    );
}

#[kithara::test(native)]
fn ascending_phase_replays_reports_descending_region(
    ascending_pcm: Vec<f32>,
    descending_pcm: Vec<f32>,
) {
    let descending_start = 128;
    let descending_len = 64;
    let mut left = ascending_pcm[..512].to_vec();
    let descending = descending_pcm;
    left[descending_start..descending_start + descending_len].copy_from_slice(&descending);

    let replays = ascending_phase_replays(&left, 0, left.len(), 3);

    assert_eq!(replays.len(), 1);
    assert_replay(
        replays[0],
        Replay {
            start_frame: descending_start,
            len: descending_len,
            start_phase: 65_535,
        },
    );
}

fn assert_replay(actual: Replay, expected: Replay) {
    assert_eq!(actual.start_frame, expected.start_frame);
    assert_eq!(actual.len, expected.len);
    assert_eq!(actual.start_phase, expected.start_phase);
}
