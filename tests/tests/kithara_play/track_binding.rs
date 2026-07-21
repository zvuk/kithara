use std::num::NonZeroU32;

use kithara::{
    audio::{
        BeatGrid, BeatMapError, SourceFrame, TrackBeat, TrackBeatMap, analysis::TrackAnalysis,
    },
    play::{PlaybackDirection, SessionBeat, SyncUnavailable, TrackBinding},
};
use kithara_integration_tests::kithara;

const SOURCE_FRAMES: u64 = 1_000_000;

fn source_frame(value: f64) -> SourceFrame {
    SourceFrame::new(value).expect("valid source frame")
}

fn sample_rate() -> NonZeroU32 {
    NonZeroU32::new(44_100).expect("non-zero sample rate")
}

fn track_beat(value: f64) -> TrackBeat {
    TrackBeat::new(value).expect("valid track beat")
}

fn session_beat(value: f64) -> SessionBeat {
    SessionBeat::new(value).expect("valid session beat")
}

fn analysis(grid: BeatGrid) -> TrackAnalysis {
    TrackAnalysis::with_source_rate(Some(grid), None, SOURCE_FRAMES, sample_rate())
}

fn analysis_at(grid: BeatGrid, source_sample_rate: NonZeroU32) -> TrackAnalysis {
    TrackAnalysis::with_source_rate(Some(grid), None, SOURCE_FRAMES, source_sample_rate)
}

#[kithara::test]
fn track_beat_map_interpolates_non_uniform_markers_and_inverts() {
    let grid = BeatGrid::new(
        120.0,
        vec![1_000, 2_000, 3_500, 5_000],
        vec![1_000],
        Vec::new(),
    );
    let map = TrackBeatMap::new(&analysis(grid), sample_rate()).expect("valid analysed markers");

    assert_eq!(
        map.source_frame_at(track_beat(1.5)),
        Some(source_frame(2_750.0))
    );
    assert_eq!(
        map.track_beat_at(source_frame(2_750.0)),
        Some(track_beat(1.5))
    );
}

#[kithara::test]
fn track_beat_map_does_not_extrapolate_outside_markers() {
    let grid = BeatGrid::new(
        120.0,
        vec![1_000, 2_000, 3_500, 5_000],
        vec![1_000],
        Vec::new(),
    );
    let map = TrackBeatMap::new(&analysis(grid), sample_rate()).expect("valid analysed markers");

    assert_eq!(map.source_frame_at(track_beat(-0.5)), None);
    assert_eq!(map.source_frame_at(track_beat(3.5)), None);
    assert_eq!(map.track_beat_at(source_frame(999.0)), None);
    assert_eq!(map.track_beat_at(source_frame(5_001.0)), None);
}

#[kithara::test]
fn track_beat_map_converts_source_rate_to_host_rate() {
    let source_sample_rate = NonZeroU32::new(48_000).expect("non-zero source rate");
    let grid = BeatGrid::new(120.0, vec![0, 24_000, 48_000], vec![0], Vec::new());
    let map = TrackBeatMap::new(&analysis_at(grid, source_sample_rate), sample_rate())
        .expect("valid source-rate conversion");

    assert_eq!(
        map.source_frame_at(track_beat(2.0)),
        Some(source_frame(44_100.0))
    );
    assert_eq!(map.source_frame_count(), 918_750);
}

#[kithara::test]
fn track_beat_map_source_extent_uses_decoder_rounding() {
    let source_rate = NonZeroU32::new(2).expect("non-zero source rate");
    let host_rate = NonZeroU32::new(1).expect("non-zero host rate");
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(120.0, vec![0, 1], vec![0], Vec::new())),
        None,
        1,
        source_rate,
    );
    let map = TrackBeatMap::new(&analysis, host_rate).expect("half-frame extent rounds upward");

    assert_eq!(map.source_frame_count(), 1);

    let exact = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(120.0, vec![0, 1], vec![0], Vec::new())),
        None,
        u64::MAX,
        host_rate,
    );
    let map = TrackBeatMap::new(&exact, host_rate).expect("equal-rate extent remains exact");
    assert_eq!(map.source_frame_count(), u64::MAX);
}

#[kithara::test]
fn track_beat_map_rejects_host_rate_source_extent_overflow() {
    let source_rate = NonZeroU32::new(1).expect("non-zero source rate");
    let host_rate = NonZeroU32::new(2).expect("non-zero host rate");
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(120.0, vec![0, 1], vec![0], Vec::new())),
        None,
        u64::MAX,
        source_rate,
    );

    assert!(matches!(
        TrackBeatMap::new(&analysis, host_rate),
        Err(BeatMapError::SourceExtentOutOfRange {
            source_frames: u64::MAX,
            source_sample_rate: 1,
            host_sample_rate: 2,
        })
    ));
}

#[kithara::test]
fn first_downbeat_defines_zero_and_preserves_pickup_beats() {
    let grid = BeatGrid::new(
        120.0,
        vec![0, 1_000, 2_000, 3_000, 4_000],
        vec![1_000, 4_000],
        Vec::new(),
    );
    let map = TrackBeatMap::new(&analysis(grid), sample_rate()).expect("valid pickup markers");

    assert_eq!(map.beats_per_bar(), Some(3));
    assert_eq!(map.track_beat_at(source_frame(0.0)), Some(track_beat(-1.0)));
    assert_eq!(
        map.source_frame_at(track_beat(0.0)),
        Some(source_frame(1_000.0))
    );
}

#[kithara::test]
fn track_beat_map_preserves_non_zero_source_anchor_and_meter() {
    const START: u64 = 11_025;
    const STEP: u64 = 22_050;

    for meter in [3_u16, 4] {
        let beats = (0..=u64::from(meter) * 3)
            .map(|beat| START + beat * STEP)
            .collect();
        let downbeats = (0..=3)
            .map(|bar| START + bar * u64::from(meter) * STEP)
            .collect();
        let grid = BeatGrid::new(120.0, beats, downbeats, Vec::new());
        let map = TrackBeatMap::new(&analysis(grid), sample_rate()).expect("valid metered markers");

        assert_eq!(
            map.source_frame_at(track_beat(0.0)),
            Some(SourceFrame::try_from(START).expect("exact source frame"))
        );
        assert_eq!(map.beats_per_bar(), Some(meter));
        assert_eq!(
            map.downbeats()
                .iter()
                .copied()
                .map(TrackBeat::get)
                .collect::<Vec<_>>(),
            vec![
                0.0,
                f64::from(meter),
                f64::from(meter * 2),
                f64::from(meter * 3)
            ]
        );
    }
}

#[kithara::test]
fn binding_keeps_signed_phase_before_non_zero_anchor() {
    let grid = BeatGrid::new(120.0, vec![0, 1_000, 2_000, 3_000], vec![0], Vec::new());
    let binding = TrackBinding::new(
        &analysis(grid),
        sample_rate(),
        session_beat(8.0),
        track_beat(2.0),
        PlaybackDirection::Forward,
    )
    .expect("analysis can be bound");

    assert_eq!(
        binding
            .track_beat_at(session_beat(7.5))
            .expect("finite phase"),
        track_beat(1.5)
    );
    assert_eq!(
        binding
            .track_beat_at(session_beat(5.0))
            .expect("finite phase"),
        track_beat(-1.0)
    );

    let reverse = TrackBinding::new(
        &analysis(BeatGrid::new(
            120.0,
            vec![0, 1_000, 2_000, 3_000],
            vec![0],
            Vec::new(),
        )),
        sample_rate(),
        session_beat(8.0),
        track_beat(2.0),
        PlaybackDirection::Reverse,
    )
    .expect("analysis can be reverse-bound");
    assert_eq!(
        reverse
            .track_beat_at(session_beat(8.5))
            .expect("finite reverse phase"),
        track_beat(1.5)
    );
}

#[kithara::test]
fn missing_invalid_and_tempo_only_analysis_are_sync_unavailable() {
    let cases = [
        TrackAnalysis::with_source_rate(None, None, SOURCE_FRAMES, sample_rate()),
        analysis(BeatGrid::new(
            120.0,
            vec![1_000, 1_000],
            vec![1_000],
            Vec::new(),
        )),
        analysis(BeatGrid::new(120.0, Vec::new(), Vec::new(), Vec::new())),
    ];

    for analysis in cases {
        let result = TrackBinding::new(
            &analysis,
            sample_rate(),
            session_beat(0.0),
            track_beat(0.0),
            PlaybackDirection::Forward,
        );
        assert!(matches!(result, Err(SyncUnavailable::BeatMap { .. })));
    }
}

#[kithara::test]
fn marker_past_decoded_source_end_is_rejected() {
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(120.0, vec![0, 1_001], vec![0], Vec::new())),
        None,
        1_000,
        sample_rate(),
    );

    assert!(matches!(
        TrackBeatMap::new(&analysis, sample_rate()),
        Err(BeatMapError::MarkerPastSourceEnd {
            index: 1,
            frame: 1_001,
            source_frames: 1_000,
        })
    ));
}
