use kithara_integration_tests::{
    cochlea::{
        host_beat_alignment_failures, marked_rhythm_markers, marked_synchronization_failures,
        synchronization_failures,
    },
    kithara,
};
use kithara_test_fixtures::{
    asset::Asset,
    assets::{
        by_name, rhythm_wav_deck_a_120bpm_48k, rhythm_wav_deck_b_120bpm_48k,
        rhythm_wav_deck_b_missing_beat_120bpm_48k, rhythm_wav_deck_b_one_beat_bar_late_120bpm_48k,
        rhythm_wav_deck_b_one_frame_late_120bpm_48k, rhythm_wav_deck_c_120bpm_48k,
        rhythm_wav_deck_d_120bpm_48k,
    },
};

const CHANNELS: u16 = 2;
const SAMPLE_RATE: u32 = 48_000;
const TARGET_BPM: f64 = 120.0;

fn samples(asset: Asset) -> Vec<f32> {
    let bytes = asset.bytes();
    let header = kithara_test_fixtures::signal::header(SAMPLE_RATE, CHANNELS, Some(0));
    bytes[header.len()..]
        .chunks_exact(size_of::<i16>())
        .map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / f32::from(i16::MAX))
        .collect()
}

fn rhythm(style: &str, control: &str) -> Vec<f32> {
    let name = format!("rhythm_wav_{style}_{control}");
    samples(by_name(&name).unwrap_or_else(|| panic!("missing `{name}`")))
}

fn rhythm_controls(style: &str) -> [Vec<f32>; 4] {
    [
        "aligned",
        "one_frame_late",
        "one_beat_bar_late",
        "missing_beat",
    ]
    .map(|control| rhythm(style, control))
}

#[kithara::test(native)]
#[case::ambient_dub(rhythm_ambient_dub_62(), "ambient_dub_62", 62.0)]
#[case::trip_hop(rhythm_trip_hop_74(), "trip_hop_74", 74.0)]
#[case::downtempo(rhythm_downtempo_96(), "downtempo_96", 96.0)]
#[case::house(rhythm_house_124(), "house_124", 124.0)]
#[case::techno(rhythm_techno_132(), "techno_132", 132.0)]
#[case::breakbeat(rhythm_breakbeat_140(), "breakbeat_140", 140.0)]
fn rich_rhythmic_oracle_covers_style_tempo_and_negative_controls(
    #[case] controls: [Vec<f32>; 4],
    #[case] style: &str,
    #[case] bpm: f64,
) {
    let [aligned, one_frame_late, one_beat_bar_late, missing_beat] = controls;

    assert!(
        marked_synchronization_failures(style, &[aligned.as_slice()], CHANNELS, SAMPLE_RATE, bpm)
            .is_empty(),
        "{style}: aligned fixture must match its declared {bpm} BPM",
    );
    assert_eq!(
        marked_synchronization_failures(
            style,
            &[aligned.as_slice(), one_frame_late.as_slice()],
            CHANNELS,
            SAMPLE_RATE,
            bpm,
        ),
        [format!("{style}: beat phase spread is 1 frame")],
    );
    assert_eq!(
        marked_synchronization_failures(
            style,
            &[aligned.as_slice(), one_beat_bar_late.as_slice()],
            CHANNELS,
            SAMPLE_RATE,
            bpm,
        ),
        [format!("{style}: bar phase spread is 1 beat")],
    );
    let missing = marked_synchronization_failures(
        style,
        &[aligned.as_slice(), missing_beat.as_slice()],
        CHANNELS,
        SAMPLE_RATE,
        bpm,
    );
    assert_eq!(
        missing.len(),
        1,
        "{style}: unexpected failures: {missing:?}"
    );
    assert!(
        missing[0].contains("track 1 is missing a rhythmic event"),
        "{style}: missing-beat control failed for the wrong reason: {missing:?}",
    );
}

#[kithara::test(native)]
fn static_rhythmic_oracle_accepts_aligned_stems_and_rejects_one_frame_phase_error(
    deck_a: Vec<f32>,
    deck_b: Vec<f32>,
    deck_c: Vec<f32>,
    deck_d: Vec<f32>,
    deck_b_one_frame_late: Vec<f32>,
) {
    let aligned = [
        deck_a.as_slice(),
        deck_b.as_slice(),
        deck_c.as_slice(),
        deck_d.as_slice(),
    ];

    assert!(
        synchronization_failures(
            "aligned static stems",
            &aligned,
            CHANNELS,
            SAMPLE_RATE,
            TARGET_BPM,
        )
        .is_empty(),
        "known-aligned build-time stems must pass the synchronization oracle",
    );

    let shifted = deck_b_one_frame_late;
    let one_frame_late = [
        deck_a.as_slice(),
        shifted.as_slice(),
        deck_c.as_slice(),
        deck_d.as_slice(),
    ];
    let failures = synchronization_failures(
        "one-frame-late static stem",
        &one_frame_late,
        CHANNELS,
        SAMPLE_RATE,
        TARGET_BPM,
    );

    assert_eq!(
        failures.as_slice(),
        ["one-frame-late static stem: beat phase spread is 1 frame"],
        "the negative control must fail only because of its exact one-frame phase error",
    );
}

#[kithara::test(native)]
#[case::missing_beat(
    Some(deck_b_missing_beat()),
    "missing static beat",
    TARGET_BPM,
    "missing static beat: track 1 is missing a rhythmic event before frame 168000",
    None
)]
#[case::wrong_tempo(
    None,
    "wrong target tempo",
    127.0,
    "tempo is",
    Some("expected 127.000")
)]
#[case::bar_phase(
    Some(deck_b_one_beat_bar_late()),
    "one-beat-late static bar",
    TARGET_BPM,
    "one-beat-late static bar: bar phase spread is 1 beat",
    None
)]
fn static_rhythmic_oracle_rejects_invalid_stems_for_the_expected_reason(
    deck_a: Vec<f32>,
    #[case] second: Option<Vec<f32>>,
    #[case] label: &str,
    #[case] target_bpm: f64,
    #[case] expected: &str,
    #[case] expected_extra: Option<&str>,
) {
    let mut tracks = vec![deck_a];
    tracks.extend(second);
    let tracks: Vec<_> = tracks.iter().map(Vec::as_slice).collect();
    let failures = synchronization_failures(label, &tracks, CHANNELS, SAMPLE_RATE, target_bpm);

    assert_eq!(failures.len(), 1, "unexpected failures: {failures:?}");
    let failure = &failures[0];
    if let Some(extra) = expected_extra {
        assert!(
            failure.contains(expected) && failure.contains(extra),
            "the control failed for an unexpected reason: {failures:?}",
        );
    } else {
        assert_eq!(failure, expected);
    }
}

#[kithara::test(native)]
#[case::sustained_tone(sustained_tone(), "sustained tone")]
#[case::white_noise(white_noise(), "white noise")]
fn estimate_oracle_rejects_a_track_without_rhythm(#[case] track: Vec<f32>, #[case] label: &str) {
    let failures = synchronization_failures(
        label,
        &[track.as_slice()],
        CHANNELS,
        SAMPLE_RATE,
        TARGET_BPM,
    );

    assert_eq!(
        failures,
        [format!("{label}: track 0 has no exact beat markers")],
        "a track with no rhythmic events must fail for that reason and no other",
    );
}

/// Frames of one short full-scale click, the event the leading-cluster rule reasons about.
const CLICK_FRAMES: usize = 96;
/// One beat at `TARGET_BPM`.
const BEAT_FRAMES: usize = 24_000;

/// Interleaved stereo clicks starting at `first`, one per beat, over two seconds.
fn clicks_from(first: usize) -> Vec<f32> {
    let frames = usize::try_from(SAMPLE_RATE).expect("sample rate fits usize") * 2;
    let mut samples = vec![0.0; frames * usize::from(CHANNELS)];
    for start in (first..frames).step_by(BEAT_FRAMES) {
        for frame in start..(start + CLICK_FRAMES).min(frames) {
            samples[frame * usize::from(CHANNELS)..][..usize::from(CHANNELS)].fill(0.9);
        }
    }
    samples
}

#[kithara::test(native)]
fn a_capture_that_opens_inside_an_event_does_not_read_its_tail_as_a_beat() {
    let mut track = clicks_from(BEAT_FRAMES);
    track[..CLICK_FRAMES / 2 * usize::from(CHANNELS)].fill(0.9);

    let (beats, _) = marked_rhythm_markers(&track, CHANNELS, SAMPLE_RATE);

    assert_eq!(beats.first(), Some(&BEAT_FRAMES));
}

#[kithara::test(native)]
fn one_exact_zero_frame_before_an_opening_peak_is_not_silence() {
    let body_frames = CLICK_FRAMES * 2;
    let mut track = clicks_from(BEAT_FRAMES);
    track[..body_frames * usize::from(CHANNELS)].fill(0.3);
    track[CLICK_FRAMES * usize::from(CHANNELS)..][..usize::from(CHANNELS)].fill(0.0);
    track[body_frames * usize::from(CHANNELS)..][..CLICK_FRAMES / 2 * usize::from(CHANNELS)]
        .fill(0.9);

    let (beats, _) = marked_rhythm_markers(&track, CHANNELS, SAMPLE_RATE);

    assert_eq!(beats.first(), Some(&BEAT_FRAMES));
}

#[kithara::test(native)]
fn an_onset_after_silence_near_the_start_is_kept_as_a_beat() {
    let onset = usize::try_from(SAMPLE_RATE).expect("sample rate fits usize") / 20;

    let (beats, _) = marked_rhythm_markers(&clicks_from(onset), CHANNELS, SAMPLE_RATE);

    assert_eq!(beats.first(), Some(&onset));
}

fn sustained_tone() -> Vec<f32> {
    let frames = SAMPLE_RATE as usize * 10;
    (0..frames)
        .flat_map(|frame| {
            let phase = std::f32::consts::TAU * 440.0 * frame as f32 / SAMPLE_RATE as f32;
            let sample = phase.sin() * 0.5;
            [sample, sample]
        })
        .collect()
}

fn white_noise() -> Vec<f32> {
    let frames = SAMPLE_RATE as usize * 10;
    let mut state = 0x9E37_79B9_u32;
    (0..frames)
        .flat_map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let sample = (state as f32 / u32::MAX as f32).mul_add(1.0, -0.5);
            [sample, sample]
        })
        .collect()
}

#[kithara::fixture]
fn deck_a() -> Vec<f32> {
    samples(rhythm_wav_deck_a_120bpm_48k())
}

#[kithara::fixture]
fn deck_b() -> Vec<f32> {
    samples(rhythm_wav_deck_b_120bpm_48k())
}

#[kithara::fixture]
fn deck_c() -> Vec<f32> {
    samples(rhythm_wav_deck_c_120bpm_48k())
}

#[kithara::fixture]
fn deck_d() -> Vec<f32> {
    samples(rhythm_wav_deck_d_120bpm_48k())
}

#[kithara::fixture]
fn deck_b_one_frame_late() -> Vec<f32> {
    samples(rhythm_wav_deck_b_one_frame_late_120bpm_48k())
}

#[kithara::fixture]
fn deck_b_missing_beat() -> Vec<f32> {
    samples(rhythm_wav_deck_b_missing_beat_120bpm_48k())
}

#[kithara::fixture]
fn deck_b_one_beat_bar_late() -> Vec<f32> {
    samples(rhythm_wav_deck_b_one_beat_bar_late_120bpm_48k())
}

#[kithara::fixture]
fn rhythm_ambient_dub_62() -> [Vec<f32>; 4] {
    rhythm_controls("ambient_dub_62")
}

#[kithara::fixture]
fn rhythm_trip_hop_74() -> [Vec<f32>; 4] {
    rhythm_controls("trip_hop_74")
}

#[kithara::fixture]
fn rhythm_downtempo_96() -> [Vec<f32>; 4] {
    rhythm_controls("downtempo_96")
}

#[kithara::fixture]
fn rhythm_house_124() -> [Vec<f32>; 4] {
    rhythm_controls("house_124")
}

#[kithara::fixture]
fn rhythm_techno_132() -> [Vec<f32>; 4] {
    rhythm_controls("techno_132")
}

#[kithara::fixture]
fn rhythm_breakbeat_140() -> [Vec<f32>; 4] {
    rhythm_controls("breakbeat_140")
}

#[kithara::test(native)]
fn host_beat_oracle_rejects_a_deck_one_frame_behind_the_host() {
    let host_beats = [BEAT_FRAMES, BEAT_FRAMES * 2, BEAT_FRAMES * 3];

    assert!(
        host_beat_alignment_failures(
            "on beat",
            &clicks_from(BEAT_FRAMES),
            CHANNELS,
            SAMPLE_RATE,
            &host_beats,
        )
        .is_empty(),
        "clicks on the Host beats must pass",
    );
    assert_eq!(
        host_beat_alignment_failures(
            "late",
            &clicks_from(BEAT_FRAMES + 1),
            CHANNELS,
            SAMPLE_RATE,
            &host_beats,
        ),
        host_beats.map(|beat| format!(
            "late: beat marker at frame {} is +1 frames from Host beat at frame {beat}",
            beat + 1,
        )),
    );
}
