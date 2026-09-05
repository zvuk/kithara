use kithara_integration_tests::{
    cochlea::{marked_synchronization_failures, synchronization_failures},
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

#[kithara::test(native, flash(false))]
#[case::ambient_dub("ambient_dub_62", 62.0)]
#[case::trip_hop("trip_hop_74", 74.0)]
#[case::downtempo("downtempo_96", 96.0)]
#[case::house("house_124", 124.0)]
#[case::techno("techno_132", 132.0)]
#[case::breakbeat("breakbeat_140", 140.0)]
fn rich_rhythmic_oracle_covers_style_tempo_and_negative_controls(
    #[case] style: &str,
    #[case] bpm: f64,
) {
    let aligned = rhythm(style, "aligned");
    let one_frame_late = rhythm(style, "one_frame_late");
    let one_beat_bar_late = rhythm(style, "one_beat_bar_late");
    let missing_beat = rhythm(style, "missing_beat");

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

#[kithara::test(native, flash(false))]
fn static_rhythmic_oracle_accepts_aligned_stems_and_rejects_one_frame_phase_error() {
    let deck_a = samples(rhythm_wav_deck_a_120bpm_48k());
    let deck_b = samples(rhythm_wav_deck_b_120bpm_48k());
    let deck_c = samples(rhythm_wav_deck_c_120bpm_48k());
    let deck_d = samples(rhythm_wav_deck_d_120bpm_48k());
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

    let shifted = samples(rhythm_wav_deck_b_one_frame_late_120bpm_48k());
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

#[kithara::test(native, flash(false))]
#[case::missing_beat(
    Some(rhythm_wav_deck_b_missing_beat_120bpm_48k()),
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
    Some(rhythm_wav_deck_b_one_beat_bar_late_120bpm_48k()),
    "one-beat-late static bar",
    TARGET_BPM,
    "one-beat-late static bar: bar phase spread is 1 beat",
    None
)]
fn static_rhythmic_oracle_rejects_invalid_stems_for_the_expected_reason(
    #[case] second: Option<Asset>,
    #[case] label: &str,
    #[case] target_bpm: f64,
    #[case] expected: &str,
    #[case] expected_extra: Option<&str>,
) {
    let mut tracks = vec![samples(rhythm_wav_deck_a_120bpm_48k())];
    tracks.extend(second.map(samples));
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
