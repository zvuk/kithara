#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use kithara::{
    assets::StoreOptions,
    audio::generate_log_spaced_bands,
    events::{AbrEvent, Event, EventReceiver},
    hls::AbrMode,
    platform::{
        sync::Arc,
        time::{self, Duration},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{PlayerConfig, PlayerImpl, Resource, ResourceConfig, StretchControls},
    queue::{Queue, QueueConfig, Transition},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::PcmPattern,
    pcm_provenance::{
        FrameClass, Replay, SAWTOOTH_PERIOD_FRAMES, ascending_phase_replays, classify_windows,
        phase_units,
    },
    temp_dir,
};

use super::offline_player_harness::OfflinePlayerHarness;

const SAMPLE_RATE: u32 = 44_100;
const RESAMPLED_RENDER_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const SEGMENTS: usize = 4;
const CROSSFADE_SEGMENTS: usize = 8;
const REAL_GEOMETRY_SEGMENTS: usize = 6;
const SEGMENT_SECS: f64 = 2.0;
const REAL_GEOMETRY_SEGMENT_SECS: f64 = 6.0;
const BLOCK_FRAMES: usize = 512;
const BLOCK_BUDGET: usize = 2_400;
const CROSSFADE_BLOCK_BUDGET: usize = 4_500;
const REAL_GEOMETRY_BLOCK_BUDGET: usize = 9_000;
const WINDOW_FRAMES: usize = 64;
const POST_ROLL_FRAMES: usize = 88_200;
const SUSTAINED_DESCENDING_WINDOWS: usize = 3;
const SUSTAINED_ASCENDING_REPLAY_WINDOWS: usize = 8;
const ASCENDING_TOL: f32 = 0.5;
const PHASE_TOL_UNITS: i32 = 3;
const TRACK_FRAME_TOLERANCE: usize = BLOCK_FRAMES * 2;
const CROSSFADE_SECS: f32 = 5.0;
const CROSSFADE_DURATION_WAIT_SECS: f64 = 12.0;
const REAL_GEOMETRY_DURATION_WAIT_SECS: f64 = 30.0;
const SEEK_OFFSET_SECS: f64 = 0.5;
const EXPECTED_POST_SEEK_FRAMES: usize = 22_050;
const SEEK_LANDING_WINDOW_FRAMES: usize = 13_230;
const SEEK_PHASE_TOL_UNITS: i32 = 3 * 512;
const MIN_RENDER_PEAK: f32 = 1.0e-6;
const TONE_A_FREQ_HZ: f64 = 440.0;
const TONE_B_FREQ_HZ: f64 = 880.0;
const TONE_WINDOW_FRAMES: usize = 1_024;
const TONE_MAG_FLOOR: f64 = 1.0;

type ClassRun = (FrameClass, usize, usize);
type ToneRun = (ToneClass, usize, usize);

struct ProvenanceDumpContext<'a> {
    replays: &'a [Replay],
    runs: &'a [ClassRun],
    onset_window: Option<usize>,
    b_onset_window: Option<usize>,
    current_index: Option<usize>,
}

impl ProvenanceDumpContext<'_> {
    fn dump(&self) -> String {
        dump(
            self.replays,
            self.runs,
            self.onset_window,
            self.b_onset_window,
            self.current_index,
        )
    }
}

struct SwitchDumpContext<'a> {
    replays: &'a [Replay],
    runs: &'a [ClassRun],
    onset_window: Option<usize>,
    b_onset_window: Option<usize>,
    switch_issue_frame: usize,
    current_index: Option<usize>,
}

impl SwitchDumpContext<'_> {
    fn dump(&self) -> String {
        dump_with_switch(
            self.replays,
            self.runs,
            self.onset_window,
            self.b_onset_window,
            self.switch_issue_frame,
            self.current_index,
        )
    }
}

struct CrossfadeDumpContext<'a> {
    raw_runs: &'a [ClassRun],
    collapsed_runs: &'a [ClassRun],
    onset_window: Option<usize>,
    b_onset_window: Option<usize>,
    expected_a_end_frame: usize,
    current_index: Option<usize>,
}

impl CrossfadeDumpContext<'_> {
    fn dump(&self) -> String {
        crossfade_dump(
            self.raw_runs,
            self.collapsed_runs,
            self.onset_window,
            self.b_onset_window,
            self.expected_a_end_frame,
            self.current_index,
        )
    }
}

struct CrossfadeAnalysis {
    raw_runs: Vec<ClassRun>,
    collapsed_runs: Vec<ClassRun>,
    onset_window: usize,
    b_onset_window: usize,
}

impl CrossfadeAnalysis {
    fn context(
        &self,
        expected_a_end_frame: usize,
        current_index: Option<usize>,
    ) -> CrossfadeDumpContext<'_> {
        CrossfadeDumpContext {
            raw_runs: &self.raw_runs,
            collapsed_runs: &self.collapsed_runs,
            onset_window: Some(self.onset_window),
            b_onset_window: Some(self.b_onset_window),
            expected_a_end_frame,
            current_index,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToneClass {
    A440,
    B880,
    Silence,
    Unknown,
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_emits_only_b_after_a_flac(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let setup = setup_queue(&server, &temp_dir, true).await;

    let (rendered, expected_a_frames) =
        render_until_b_with_postroll(&setup.queue, &setup.harness, ASCENDING_TOL, SAMPLE_RATE)
            .await;
    let left_raw = deinterleave_left(&rendered, usize::from(CHANNELS));
    let left = normalized_left(&left_raw, setup.queue.current_index());
    let classes = classify_windows(&left, WINDOW_FRAMES, ASCENDING_TOL);
    let runs = class_runs(&classes);
    let onset_window = require_first_non_silence(&classes, &runs, setup.queue.current_index());
    let b_onset_window = require_first_sustained_descending_window(
        &classes,
        0,
        &runs,
        Some(onset_window),
        setup.queue.current_index(),
    );
    let onset_frame = frame_for_window(onset_window);
    let phase_start_frame = onset_frame.saturating_add(WINDOW_FRAMES);
    let search_context = ProvenanceDumpContext {
        replays: &[],
        runs: &runs,
        onset_window: Some(onset_window),
        b_onset_window: Some(b_onset_window),
        current_index: setup.queue.current_index(),
    };
    let last_ascending_window = require_last_class_window_before(
        &classes,
        FrameClass::Ascending,
        b_onset_window,
        &search_context,
    );
    let last_ascending_end_frame = frame_for_window(last_ascending_window + 1);
    let replays = ascending_phase_replays(
        &left,
        phase_start_frame,
        last_ascending_end_frame,
        PHASE_TOL_UNITS,
    );
    let context = ProvenanceDumpContext {
        replays: &replays,
        runs: &runs,
        onset_window: Some(onset_window),
        b_onset_window: Some(b_onset_window),
        current_index: setup.queue.current_index(),
    };

    let _first_ascending_window = require_ascending_near_onset(
        &classes,
        onset_window,
        "track A ascending content must start near onset",
        &dump(
            &replays,
            &runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index(),
        ),
    );
    assert!(
        replays.is_empty(),
        "track A phase must be monotonic until B starts; {}",
        dump(
            &replays,
            &runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );

    assert_ascending_len(&classes, expected_a_frames, &context);
    assert_no_ascending_after_b(&classes, &context);
    assert_eq!(
        setup.queue.current_index(),
        Some(1),
        "queue.current_index must advance to track B; {}",
        dump(
            &replays,
            &runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_with_late_variant_switch_flac(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let setup = setup_multivariant_flac_queue(&server, &temp_dir).await;

    let (rendered, expected_a_frames, switch_issue_frame, committed_variant) =
        render_until_b_with_late_variant_switch(&setup.queue, &setup.harness).await;
    let left_raw = deinterleave_left(&rendered, usize::from(CHANNELS));
    let left = normalized_left(&left_raw, setup.queue.current_index());
    let classes = classify_windows(&left, WINDOW_FRAMES, ASCENDING_TOL);
    let runs = class_runs(&classes);
    let onset_window = require_first_non_silence_with_switch(
        &classes,
        &runs,
        switch_issue_frame,
        setup.queue.current_index(),
    );
    let b_onset_window = require_first_sustained_descending_window_with_switch(
        &classes,
        0,
        &runs,
        Some(onset_window),
        switch_issue_frame,
        setup.queue.current_index(),
    );
    let onset_frame = frame_for_window(onset_window);
    let phase_start_frame = onset_frame.saturating_add(WINDOW_FRAMES);
    let search_context = SwitchDumpContext {
        replays: &[],
        runs: &runs,
        onset_window: Some(onset_window),
        b_onset_window: Some(b_onset_window),
        switch_issue_frame,
        current_index: setup.queue.current_index(),
    };
    let last_ascending_window = require_last_class_window_before_with_switch(
        &classes,
        FrameClass::Ascending,
        b_onset_window,
        &search_context,
    );
    let last_ascending_end_frame = frame_for_window(last_ascending_window + 1);
    let replays = ascending_phase_replays(
        &left,
        phase_start_frame,
        last_ascending_end_frame,
        PHASE_TOL_UNITS,
    );
    let context = SwitchDumpContext {
        replays: &replays,
        runs: &runs,
        onset_window: Some(onset_window),
        b_onset_window: Some(b_onset_window),
        switch_issue_frame,
        current_index: setup.queue.current_index(),
    };

    assert_eq!(
        committed_variant,
        Some(1),
        "manual switch never committed -- case is vacuous; move the switch earlier; \
         observed_committed_variant={committed_variant:?}; {}",
        dump_with_switch(
            &replays,
            &runs,
            Some(onset_window),
            Some(b_onset_window),
            switch_issue_frame,
            setup.queue.current_index(),
        )
    );

    let _first_ascending_window = require_ascending_near_onset(
        &classes,
        onset_window,
        "variant-switch track A ascending content must start near onset",
        &dump_with_switch(
            &replays,
            &runs,
            Some(onset_window),
            Some(b_onset_window),
            switch_issue_frame,
            setup.queue.current_index(),
        ),
    );
    assert!(
        replays.is_empty(),
        "variant-switch track A phase must be monotonic until B starts; {}",
        dump_with_switch(
            &replays,
            &runs,
            Some(onset_window),
            Some(b_onset_window),
            switch_issue_frame,
            setup.queue.current_index(),
        )
    );

    assert_ascending_len_with_switch(&classes, expected_a_frames, &context);
    assert_no_ascending_after_b_with_switch(&classes, &context);
    assert_eq!(
        setup.queue.current_index(),
        Some(1),
        "queue.current_index must advance to track B after late variant switch; {}",
        dump_with_switch(
            &replays,
            &runs,
            Some(onset_window),
            Some(b_onset_window),
            switch_issue_frame,
            setup.queue.current_index(),
        )
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_app_layer_crossfade_advance_flac_resampled_48k(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let timestretch = StretchControls::new(1.0);
    let setup = setup_flac_queue_with_player_config_autoplay(
        &server,
        &temp_dir,
        RESAMPLED_RENDER_RATE,
        CROSSFADE_SEGMENTS,
        crossfade_eq_stretch_player_config(&timestretch),
        true,
    )
    .await;

    let (rendered, expected_a_end_frame) = render_app_layer_crossfade_until_b_with_postroll(
        &setup.queue,
        &setup.harness,
        RESAMPLED_RENDER_RATE,
    )
    .await;
    let analysis = assert_crossfade_contract(
        &rendered,
        &setup.queue,
        expected_a_end_frame,
        RESAMPLED_RENDER_RATE,
        collapse_resampled_noise_islands,
        "app-layer crossfade resampled FLAC",
    );
    let context = analysis.context(expected_a_end_frame, setup.queue.current_index());

    assert_no_sustained_ascending_after_b_onset(&context, "app-layer crossfade resampled FLAC");
    assert_eq!(
        setup.queue.current_index(),
        Some(1),
        "app-layer crossfade resampled FLAC queue.current_index must advance to track B at end; {}",
        context.dump()
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(240)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_app_layer_crossfade_advance_flac_resampled_48k_real_geometry(
    temp_dir: TestTempDir,
) {
    let server = TestServerHelper::new().await;
    let timestretch = StretchControls::new(1.0);
    let setup = setup_flac_queue_with_player_config_autoplay_geometry(
        &server,
        &temp_dir,
        RESAMPLED_RENDER_RATE,
        REAL_GEOMETRY_SEGMENTS,
        REAL_GEOMETRY_SEGMENT_SECS,
        crossfade_eq_stretch_player_config(&timestretch),
        true,
    )
    .await;

    let (rendered, expected_a_end_frame) = render_app_layer_crossfade_until_b_with_postroll_config(
        &setup.queue,
        &setup.harness,
        RESAMPLED_RENDER_RATE,
        REAL_GEOMETRY_BLOCK_BUDGET,
        REAL_GEOMETRY_DURATION_WAIT_SECS,
    )
    .await;
    let analysis = assert_crossfade_contract(
        &rendered,
        &setup.queue,
        expected_a_end_frame,
        RESAMPLED_RENDER_RATE,
        collapse_resampled_noise_islands,
        "app-layer crossfade resampled FLAC real geometry",
    );
    let context = analysis.context(expected_a_end_frame, setup.queue.current_index());

    assert_no_sustained_ascending_after_b_onset(
        &context,
        "app-layer crossfade resampled FLAC real geometry",
    );
    assert_eq!(
        setup.queue.current_index(),
        Some(1),
        "app-layer crossfade resampled FLAC real geometry queue.current_index must advance to track B at end; {}",
        context.dump()
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_emits_only_b_flac_resampled_48k(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let setup = setup_queue_with_sample_rate(&server, &temp_dir, true, RESAMPLED_RENDER_RATE).await;

    let (rendered, expected_a_frames) = render_until_b_with_postroll(
        &setup.queue,
        &setup.harness,
        ASCENDING_TOL,
        RESAMPLED_RENDER_RATE,
    )
    .await;
    let left_raw = deinterleave_left(&rendered, usize::from(CHANNELS));
    let left = normalized_left(&left_raw, setup.queue.current_index());
    let classes = classify_windows(&left, WINDOW_FRAMES, ASCENDING_TOL);
    let raw_runs = class_runs(&classes);
    let collapsed_runs = collapse_resampled_noise_islands(&raw_runs);
    let replays: Vec<Replay> = Vec::new();
    let onset_window = require_first_non_silence(&classes, &raw_runs, setup.queue.current_index());
    let b_onset_window = require_first_sustained_descending_window(
        &classes,
        0,
        &raw_runs,
        Some(onset_window),
        setup.queue.current_index(),
    );
    let _first_ascending_window = require_ascending_near_onset(
        &classes,
        onset_window,
        "resampled FLAC ascending content must start near onset",
        &dump_with_collapsed(
            &replays,
            &raw_runs,
            &collapsed_runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index(),
        ),
    );
    let ascending_runs: Vec<ClassRun> = collapsed_runs
        .iter()
        .copied()
        .filter(|run| run.0 == FrameClass::Ascending)
        .collect();
    let descending_runs: Vec<ClassRun> = collapsed_runs
        .iter()
        .copied()
        .filter(|run| run.0 == FrameClass::Descending)
        .collect();
    let context = ProvenanceDumpContext {
        replays: &replays,
        runs: &raw_runs,
        onset_window: Some(onset_window),
        b_onset_window: Some(b_onset_window),
        current_index: setup.queue.current_index(),
    };

    assert_eq!(
        ascending_runs.len(),
        1,
        "resampled FLAC must contain exactly one maximal Ascending A block; {}",
        dump_with_collapsed(
            &replays,
            &raw_runs,
            &collapsed_runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );
    assert_eq!(
        descending_runs.len(),
        1,
        "resampled FLAC must contain exactly one maximal Descending B block; {}",
        dump_with_collapsed(
            &replays,
            &raw_runs,
            &collapsed_runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );
    assert!(
        ascending_runs[0].1 < descending_runs[0].1,
        "resampled FLAC Ascending A block must precede Descending B block; {}",
        dump_with_collapsed(
            &replays,
            &raw_runs,
            &collapsed_runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );
    assert_close_len(
        ascending_runs[0].2 * WINDOW_FRAMES,
        expected_a_frames,
        usize::try_from(RESAMPLED_RENDER_RATE / 10).expect("render-rate tolerance fits usize"),
        "resampled FLAC A length must match runtime duration at 48 kHz",
        &context,
    );
    assert!(
        no_audio_class_after_descending(&collapsed_runs, FrameClass::Ascending),
        "resampled FLAC collapsed class sequence must not contain Ascending after Descending; {}",
        dump_with_collapsed(
            &replays,
            &raw_runs,
            &collapsed_runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );
    assert_eq!(
        setup.queue.current_index(),
        Some(1),
        "queue.current_index must advance to track B for resampled FLAC; {}",
        dump_with_collapsed(
            &replays,
            &raw_runs,
            &collapsed_runs,
            Some(onset_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_emits_only_b_flac_crossfade_5s(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    run_crossfade_flac_case(
        &server,
        &temp_dir,
        SAMPLE_RATE,
        collapse_short_unknown_islands,
        "crossfade FLAC",
        crossfade_player_config,
    )
    .await;
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_emits_only_b_flac_crossfade_5s_eq(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    run_crossfade_flac_case(
        &server,
        &temp_dir,
        SAMPLE_RATE,
        collapse_short_unknown_islands,
        "crossfade FLAC eq",
        crossfade_eq_player_config,
    )
    .await;
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_emits_only_b_flac_crossfade_5s_eq_stretch(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let timestretch = StretchControls::new(1.0);

    run_crossfade_flac_case(
        &server,
        &temp_dir,
        SAMPLE_RATE,
        collapse_short_unknown_islands,
        "crossfade FLAC eq stretch",
        || crossfade_eq_stretch_player_config(&timestretch),
    )
    .await;
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_app_layer_crossfade_advance_flac(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let timestretch = StretchControls::new(1.0);
    let setup = setup_flac_queue_with_player_config_autoplay(
        &server,
        &temp_dir,
        SAMPLE_RATE,
        CROSSFADE_SEGMENTS,
        crossfade_eq_stretch_player_config(&timestretch),
        true,
    )
    .await;

    let (rendered, expected_a_end_frame) =
        render_app_layer_crossfade_until_b_with_postroll(&setup.queue, &setup.harness, SAMPLE_RATE)
            .await;
    let analysis = assert_crossfade_contract(
        &rendered,
        &setup.queue,
        expected_a_end_frame,
        SAMPLE_RATE,
        collapse_short_unknown_islands,
        "app-layer crossfade FLAC",
    );
    let context = analysis.context(expected_a_end_frame, setup.queue.current_index());

    assert_no_sustained_ascending_after_b_onset(&context, "app-layer crossfade FLAC");
    assert_eq!(
        setup.queue.current_index(),
        Some(1),
        "app-layer crossfade FLAC queue.current_index must advance to track B at end; {}",
        context.dump()
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_emits_only_b_flac_crossfade_5s_resampled_48k(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    run_crossfade_flac_case(
        &server,
        &temp_dir,
        RESAMPLED_RENDER_RATE,
        collapse_resampled_noise_islands,
        "crossfade resampled FLAC",
        crossfade_player_config,
    )
    .await;
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn seek_near_end_then_eof_advance_emits_only_b_flac(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let setup = setup_queue(&server, &temp_dir, true).await;

    let (rendered, seek_issue_frame, duration) =
        render_seek_near_end_until_b_with_postroll(&setup.queue, &setup.harness, SAMPLE_RATE).await;
    let left_raw = deinterleave_left(&rendered, usize::from(CHANNELS));
    let left = normalized_left(&left_raw, setup.queue.current_index());
    let classes = classify_windows(&left, WINDOW_FRAMES, ASCENDING_TOL);
    let runs = class_runs(&classes);
    let b_onset_window = require_first_sustained_descending_window(
        &classes,
        seek_issue_frame,
        &runs,
        None,
        setup.queue.current_index(),
    );
    let b_onset_frame = frame_for_window(b_onset_window);
    let landing_frame = find_seek_landing_frame(&left, seek_issue_frame, b_onset_frame, duration)
        .unwrap_or_else(|| {
            panic!(
                "seek landing must reach the expected near-EOF phase; \
                     seek_issue_frame={seek_issue_frame}; {}",
                dump(
                    &[],
                    &runs,
                    None,
                    Some(b_onset_window),
                    setup.queue.current_index()
                )
            )
        });
    let landing_window = landing_frame / WINDOW_FRAMES;
    let search_context = ProvenanceDumpContext {
        replays: &[],
        runs: &runs,
        onset_window: Some(landing_window),
        b_onset_window: Some(b_onset_window),
        current_index: setup.queue.current_index(),
    };
    let last_ascending_window = require_last_class_window_before(
        &classes,
        FrameClass::Ascending,
        b_onset_window,
        &search_context,
    );
    let last_ascending_end_frame = frame_for_window(last_ascending_window + 1);

    let phase_start_frame = landing_frame.saturating_add(WINDOW_FRAMES);
    let replays = ascending_phase_replays(
        &left,
        phase_start_frame,
        last_ascending_end_frame,
        PHASE_TOL_UNITS,
    );
    let context = ProvenanceDumpContext {
        replays: &replays,
        runs: &runs,
        onset_window: Some(landing_window),
        b_onset_window: Some(b_onset_window),
        current_index: setup.queue.current_index(),
    };
    assert!(
        replays.is_empty(),
        "post-seek track A phase must be monotonic until B starts; \
         seek_issue_frame={seek_issue_frame}; {}",
        dump(
            &replays,
            &runs,
            Some(landing_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );

    let ascending_frames = last_ascending_end_frame.saturating_sub(landing_frame);
    assert_close_len(
        ascending_frames,
        EXPECTED_POST_SEEK_FRAMES,
        TRACK_FRAME_TOLERANCE,
        "post-seek ascending length must be approximately 0.5s before B starts",
        &context,
    );
    assert_no_ascending_after_b(&classes, &context);
    assert_eq!(
        setup.queue.current_index(),
        Some(1),
        "queue.current_index must advance to track B after near-EOF seek; {}",
        dump(
            &replays,
            &runs,
            Some(landing_window),
            Some(b_onset_window),
            setup.queue.current_index()
        )
    );
}

/// AAC cannot preserve the 0.67 Hz sawtooth slope/phase provenance reliably;
/// this case uses 440 Hz vs 880 Hz tone provenance to keep the same replay
/// contract on the lossy codec.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn natural_eof_advance_emits_only_b_aac(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let setup = setup_sine_aac_queue(&server, &temp_dir).await;

    let (rendered, duration) =
        render_until_tone_b_with_postroll(&setup.queue, &setup.harness, SAMPLE_RATE).await;
    let left = deinterleave_left(&rendered, usize::from(CHANNELS));
    let classes = classify_tone_windows(&left, TONE_WINDOW_FRAMES, SAMPLE_RATE);
    let runs = tone_runs(&classes);
    let onset_window = require_first_tone_window(
        &classes,
        ToneClass::A440,
        0,
        &runs,
        None,
        setup.queue.current_index(),
    );
    let b_onset_window = require_first_sustained_tone_window(
        &classes,
        ToneClass::B880,
        0,
        &runs,
        Some(onset_window),
        setup.queue.current_index(),
    );
    let merged_runs = merge_tone_underrun_gaps(&runs);
    let diagnostics = || {
        format!(
            "{}; merged_tone_runs(class,start_window,len_without_silence_gaps)={merged_runs:?}",
            tone_dump(
                &runs,
                Some(onset_window),
                Some(b_onset_window),
                setup.queue.current_index(),
            )
        )
    };
    let a_runs: Vec<ToneRun> = merged_runs
        .iter()
        .copied()
        .filter(|run| run.0 == ToneClass::A440)
        .collect();
    let b_runs: Vec<ToneRun> = merged_runs
        .iter()
        .copied()
        .filter(|run| run.0 == ToneClass::B880)
        .collect();
    let a_content_frames = a_runs
        .iter()
        .map(|run| run.2)
        .sum::<usize>()
        .saturating_mul(TONE_WINDOW_FRAMES);
    let expected_a_frames = frames_from_secs(duration, SAMPLE_RATE);
    let max_a_content_frames = expected_a_frames.saturating_add(frames_from_secs(0.5, SAMPLE_RATE));

    assert_eq!(
        classes[onset_window],
        ToneClass::A440,
        "AAC first tone window must be 440 Hz A content; {}",
        diagnostics()
    );
    assert_eq!(
        a_runs.len(),
        1,
        "AAC provenance must contain exactly one maximal 440 Hz A region; {}",
        diagnostics()
    );
    assert_eq!(
        b_runs.len(),
        1,
        "AAC provenance must contain exactly one maximal 880 Hz B region; {}",
        diagnostics()
    );
    assert!(
        a_content_frames <= max_a_content_frames,
        "AAC 440 Hz A content must not exceed duration plus 0.5s slack; \
         a_content_frames={a_content_frames}; max_a_content_frames={max_a_content_frames}; \
         duration_seconds={duration}; {}",
        diagnostics()
    );
    assert!(
        a_runs[0].1 < b_runs[0].1,
        "AAC 440 Hz A region must precede 880 Hz B region; {}",
        diagnostics()
    );
    assert!(
        no_tone_after_b(&classes, b_onset_window, ToneClass::A440),
        "AAC B region must not contain 440 Hz A windows after B onset; {}",
        diagnostics()
    );
    assert_eq!(
        setup.queue.current_index(),
        Some(1),
        "queue.current_index must advance to track B for AAC; {}",
        diagnostics()
    );
}

struct QueueSetup {
    harness: OfflinePlayerHarness,
    queue: Queue,
}

struct RenderProgress {
    rendered: Vec<f32>,
    left: Vec<f32>,
    classes: Vec<FrameClass>,
    peak: f32,
    descending_seen_at: Option<usize>,
}

fn with_autoplay(mut config: QueueConfig, should_autoplay: bool) -> QueueConfig {
    config.should_autoplay = should_autoplay;
    config
}

async fn run_crossfade_flac_case(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    render_sample_rate: u32,
    collapse_runs: fn(&[ClassRun]) -> Vec<ClassRun>,
    label: &str,
    build_player_config: impl FnOnce() -> PlayerConfig,
) {
    let setup = setup_flac_queue_with_player_config(
        server,
        temp_dir,
        render_sample_rate,
        CROSSFADE_SEGMENTS,
        build_player_config(),
    )
    .await;

    let (rendered, expected_a_end_frame) =
        render_crossfade_until_b_with_postroll(&setup.queue, &setup.harness, render_sample_rate)
            .await;

    assert_crossfade_contract(
        &rendered,
        &setup.queue,
        expected_a_end_frame,
        render_sample_rate,
        collapse_runs,
        label,
    );
}

fn crossfade_player_config() -> PlayerConfig {
    PlayerConfig::builder()
        .crossfade_duration(CROSSFADE_SECS)
        .build()
}

fn crossfade_eq_player_config() -> PlayerConfig {
    PlayerConfig::builder()
        .crossfade_duration(CROSSFADE_SECS)
        .eq_layout(generate_log_spaced_bands(10))
        .build()
}

fn crossfade_eq_stretch_player_config(timestretch: &Arc<StretchControls>) -> PlayerConfig {
    PlayerConfig::builder()
        .crossfade_duration(CROSSFADE_SECS)
        .eq_layout(generate_log_spaced_bands(10))
        .timestretch(Arc::clone(timestretch))
        .build()
}

async fn setup_queue(server: &TestServerHelper, temp_dir: &TestTempDir, flac: bool) -> QueueSetup {
    setup_queue_with_sample_rate(server, temp_dir, flac, SAMPLE_RATE).await
}

async fn setup_queue_with_sample_rate(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    flac: bool,
    render_sample_rate: u32,
) -> QueueSetup {
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder().crossfade_duration(0.0).build(),
        render_sample_rate,
    );
    let queue = Queue::new(with_autoplay(
        QueueConfig::default().with_player(Arc::clone(harness.player())),
        false,
    ));

    let resource_a = hls_resource(
        harness.player(),
        server,
        &temp_dir.path().join("a"),
        PcmPattern::Ascending,
        flac,
    )
    .await;
    let resource_b = hls_resource(
        harness.player(),
        server,
        &temp_dir.path().join("b"),
        PcmPattern::Descending,
        flac,
    )
    .await;

    let id_a = queue.insert_loaded_for_test(resource_a);
    let _ = queue.insert_loaded_for_test(resource_b);
    queue
        .select(id_a, Transition::None)
        .expect("select track A");

    QueueSetup { harness, queue }
}

async fn setup_multivariant_flac_queue(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
) -> QueueSetup {
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder().crossfade_duration(0.0).build(),
        SAMPLE_RATE,
    );
    let queue = Queue::new(with_autoplay(
        QueueConfig::default().with_player(Arc::clone(harness.player())),
        false,
    ));

    let resource_a = hls_multivariant_flac_resource(
        harness.player(),
        server,
        &temp_dir.path().join("a"),
        PcmPattern::Ascending,
    )
    .await;
    let resource_b = hls_multivariant_flac_resource(
        harness.player(),
        server,
        &temp_dir.path().join("b"),
        PcmPattern::Descending,
    )
    .await;

    let id_a = queue.insert_loaded_for_test(resource_a);
    let _ = queue.insert_loaded_for_test(resource_b);
    queue
        .select(id_a, Transition::None)
        .expect("select track A");

    QueueSetup { harness, queue }
}

async fn setup_flac_queue_with_player_config(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    render_sample_rate: u32,
    segments: usize,
    player_config: PlayerConfig,
) -> QueueSetup {
    setup_flac_queue_with_player_config_autoplay(
        server,
        temp_dir,
        render_sample_rate,
        segments,
        player_config,
        false,
    )
    .await
}

async fn setup_flac_queue_with_player_config_autoplay(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    render_sample_rate: u32,
    segments: usize,
    player_config: PlayerConfig,
    should_autoplay: bool,
) -> QueueSetup {
    setup_flac_queue_with_player_config_autoplay_geometry(
        server,
        temp_dir,
        render_sample_rate,
        segments,
        SEGMENT_SECS,
        player_config,
        should_autoplay,
    )
    .await
}

async fn setup_flac_queue_with_player_config_autoplay_geometry(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    render_sample_rate: u32,
    segments: usize,
    segment_duration_secs: f64,
    player_config: PlayerConfig,
    should_autoplay: bool,
) -> QueueSetup {
    let harness = OfflinePlayerHarness::with_sample_rate(player_config, render_sample_rate);
    let queue = Queue::new(with_autoplay(
        QueueConfig::default().with_player(Arc::clone(harness.player())),
        should_autoplay,
    ));

    let resource_a = hls_resource_with_segments_and_duration(
        harness.player(),
        server,
        &temp_dir.path().join("a"),
        PcmPattern::Ascending,
        true,
        segments,
        segment_duration_secs,
    )
    .await;
    let resource_b = hls_resource_with_segments_and_duration(
        harness.player(),
        server,
        &temp_dir.path().join("b"),
        PcmPattern::Descending,
        true,
        segments,
        segment_duration_secs,
    )
    .await;

    if should_autoplay {
        let id_a = queue.register_for_test();
        let id_b = queue.register_for_test();
        queue.complete_load_for_test(id_b, resource_b);
        queue.complete_load_for_test(id_a, resource_a);
    } else {
        let id_a = queue.insert_loaded_for_test(resource_a);
        let _ = queue.insert_loaded_for_test(resource_b);
        queue
            .select(id_a, Transition::None)
            .expect("select track A");
    }

    QueueSetup { harness, queue }
}

async fn setup_sine_aac_queue(server: &TestServerHelper, temp_dir: &TestTempDir) -> QueueSetup {
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder().crossfade_duration(0.0).build(),
        SAMPLE_RATE,
    );
    let queue = Queue::new(with_autoplay(
        QueueConfig::default().with_player(Arc::clone(harness.player())),
        false,
    ));

    let resource_a = hls_sine_aac_resource(
        harness.player(),
        server,
        &temp_dir.path().join("a"),
        TONE_A_FREQ_HZ,
    )
    .await;
    let resource_b = hls_sine_aac_resource(
        harness.player(),
        server,
        &temp_dir.path().join("b"),
        TONE_B_FREQ_HZ,
    )
    .await;

    let id_a = queue.insert_loaded_for_test(resource_a);
    let _ = queue.insert_loaded_for_test(resource_b);
    queue
        .select(id_a, Transition::None)
        .expect("select track A");

    QueueSetup { harness, queue }
}

async fn hls_resource(
    player: &PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &Path,
    pattern: PcmPattern,
    flac: bool,
) -> Resource {
    hls_resource_with_segments(player, server, cache_dir, pattern, flac, SEGMENTS).await
}

async fn hls_resource_with_segments(
    player: &PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &Path,
    pattern: PcmPattern,
    flac: bool,
    segments: usize,
) -> Resource {
    hls_resource_with_segments_and_duration(
        player,
        server,
        cache_dir,
        pattern,
        flac,
        segments,
        SEGMENT_SECS,
    )
    .await
}

async fn hls_resource_with_segments_and_duration(
    player: &PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &Path,
    pattern: PcmPattern,
    flac: bool,
    segments: usize,
    segment_duration_secs: f64,
) -> Resource {
    let builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(segments)
        .segment_duration_secs(segment_duration_secs);
    let builder = if flac {
        builder.packaged_audio_per_variant_pcm_flac(SAMPLE_RATE, CHANNELS, vec![pattern])
    } else {
        builder.packaged_audio_per_variant_pcm_aac_lc(SAMPLE_RATE, CHANNELS, vec![pattern])
    };
    let created = server
        .create_hls(builder)
        .await
        .expect("create advance-boundary HLS fixture");
    let store = StoreOptions::new(cache_dir);
    let mut config = ResourceConfig::for_src(created.master_url().as_str())
        .expect("valid HLS master URL")
        .store(store)
        .build();
    config = player.prepare_config(config);
    let mut resource = Resource::new(config)
        .await
        .expect("open HLS resource for advance-boundary fixture");
    let _ = resource.preload().await;
    resource
}

async fn hls_multivariant_flac_resource(
    player: &PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &Path,
    pattern: PcmPattern,
) -> Resource {
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(3)
                .segments_per_variant(SEGMENTS)
                .segment_duration_secs(SEGMENT_SECS)
                .packaged_audio_per_variant_pcm_flac(
                    SAMPLE_RATE,
                    CHANNELS,
                    vec![pattern, pattern, pattern],
                ),
        )
        .await
        .expect("create advance-boundary multivariant FLAC HLS fixture");
    let store = StoreOptions::new(cache_dir);
    let mut config = ResourceConfig::for_src(created.master_url().as_str())
        .expect("valid HLS master URL")
        .store(store)
        .build();
    config = player.prepare_config(config);
    let mut resource = Resource::new(config)
        .await
        .expect("open HLS multivariant FLAC resource for advance-boundary fixture");
    let _ = resource.preload().await;
    resource
}

async fn hls_sine_aac_resource(
    player: &PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &Path,
    freq_hz: f64,
) -> Resource {
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(SEGMENTS)
                .segment_duration_secs(SEGMENT_SECS)
                .packaged_audio_sine_aac_lc(SAMPLE_RATE, CHANNELS, freq_hz),
        )
        .await
        .expect("create advance-boundary sine AAC HLS fixture");
    let store = StoreOptions::new(cache_dir);
    let mut config = ResourceConfig::for_src(created.master_url().as_str())
        .expect("valid HLS master URL")
        .store(store)
        .build();
    config = player.prepare_config(config);
    let mut resource = Resource::new(config)
        .await
        .expect("open HLS sine AAC resource for advance-boundary fixture");
    let _ = resource.preload().await;
    resource
}

async fn render_until_b_with_postroll(
    queue: &Queue,
    harness: &OfflinePlayerHarness,
    class_tolerance: f32,
    render_sample_rate: u32,
) -> (Vec<f32>, usize) {
    let mut progress = RenderProgress::new();
    let mut expected_a_frames: Option<usize> = None;

    for _ in 0..BLOCK_BUDGET {
        let _ = queue.tick();
        let block = harness.render(BLOCK_FRAMES);
        progress.push_block(&block, class_tolerance, Some(0));

        if expected_a_frames.is_none()
            && let Some(duration) = queue.duration_seconds()
            && duration > 7.0
        {
            expected_a_frames = Some(frames_from_secs(duration, render_sample_rate));
        }

        time::sleep(Duration::from_millis(1)).await;

        if progress.has_b_postroll() {
            break;
        }
    }

    (
        progress.rendered,
        expected_a_frames.expect("track A duration must be reported before EOF"),
    )
}

async fn render_until_b_with_late_variant_switch(
    queue: &Queue,
    harness: &OfflinePlayerHarness,
) -> (Vec<f32>, usize, usize, Option<usize>) {
    let mut progress = RenderProgress::new();
    let mut events = queue.subscribe();
    let mut expected_a_frames: Option<usize> = None;
    let mut switch_issue_frame: Option<usize> = None;
    let mut committed_variant: Option<usize> = None;

    for _ in 0..BLOCK_BUDGET {
        let _ = queue.tick();
        let block = harness.render(BLOCK_FRAMES);
        progress.push_block(&block, ASCENDING_TOL, Some(0));

        if expected_a_frames.is_none()
            && let Some(duration) = queue.duration_seconds()
            && duration > 7.0
        {
            expected_a_frames = Some(frames_from_secs(duration, SAMPLE_RATE));
        }

        drain_variant_applied_events(
            &mut events,
            &mut committed_variant,
            switch_issue_frame.is_some(),
        );

        if switch_issue_frame.is_none()
            && let Some(duration) = queue.duration_seconds()
            && duration > 7.0
        {
            let switch_frame = frames_from_secs(duration - 2.5, SAMPLE_RATE);
            if progress.rendered_frames() >= switch_frame {
                let handle = harness
                    .player()
                    .current_abr_handle()
                    .expect("track A HLS stream must expose AbrHandle");
                handle
                    .set_mode(AbrMode::manual(1))
                    .expect("Manual(1) target must be valid for the 3-variant fixture");
                switch_issue_frame = Some(progress.rendered_frames());
            }
        }

        drain_variant_applied_events(
            &mut events,
            &mut committed_variant,
            switch_issue_frame.is_some(),
        );

        time::sleep(Duration::from_millis(1)).await;
        drain_variant_applied_events(
            &mut events,
            &mut committed_variant,
            switch_issue_frame.is_some(),
        );

        if progress.has_b_postroll() {
            break;
        }
    }

    (
        progress.rendered,
        expected_a_frames.expect("track A duration must be reported before EOF"),
        switch_issue_frame.expect("late variant switch must be issued before EOF"),
        committed_variant,
    )
}

async fn render_crossfade_until_b_with_postroll(
    queue: &Queue,
    harness: &OfflinePlayerHarness,
    render_sample_rate: u32,
) -> (Vec<f32>, usize) {
    let mut progress = RenderProgress::new();
    let mut expected_a_end_frame: Option<usize> = None;

    for _ in 0..CROSSFADE_BLOCK_BUDGET {
        let _ = queue.tick();
        let block = harness.render(BLOCK_FRAMES);
        progress.push_block(&block, ASCENDING_TOL, Some(0));

        if expected_a_end_frame.is_none()
            && let Some(duration) = queue.duration_seconds()
            && duration > CROSSFADE_DURATION_WAIT_SECS
        {
            expected_a_end_frame = Some(frames_from_secs(duration, render_sample_rate));
        }

        time::sleep(Duration::from_millis(1)).await;

        if expected_a_end_frame.is_some() && progress.has_b_postroll() {
            break;
        }
    }

    (
        progress.rendered,
        expected_a_end_frame.expect("crossfade track A duration must be reported before EOF"),
    )
}

async fn render_app_layer_crossfade_until_b_with_postroll(
    queue: &Queue,
    harness: &OfflinePlayerHarness,
    render_sample_rate: u32,
) -> (Vec<f32>, usize) {
    render_app_layer_crossfade_until_b_with_postroll_config(
        queue,
        harness,
        render_sample_rate,
        CROSSFADE_BLOCK_BUDGET,
        CROSSFADE_DURATION_WAIT_SECS,
    )
    .await
}

async fn render_app_layer_crossfade_until_b_with_postroll_config(
    queue: &Queue,
    harness: &OfflinePlayerHarness,
    render_sample_rate: u32,
    block_budget: usize,
    duration_wait_secs: f64,
) -> (Vec<f32>, usize) {
    let mut progress = RenderProgress::new();
    let mut expected_a_end_frame: Option<usize> = None;
    let mut auto_advanced_index: Option<usize> = None;

    for _ in 0..block_budget {
        let _ = queue.tick();
        drive_app_layer_crossfade_advance(queue, &mut auto_advanced_index);

        let block = harness.render(BLOCK_FRAMES);
        progress.push_block(&block, ASCENDING_TOL, Some(0));

        if expected_a_end_frame.is_none()
            && let Some(duration) = queue.duration_seconds()
            && duration > duration_wait_secs
        {
            expected_a_end_frame = Some(frames_from_secs(duration, render_sample_rate));
        }

        time::sleep(Duration::from_millis(1)).await;

        if expected_a_end_frame.is_some() && progress.has_b_postroll() {
            break;
        }
    }

    (
        progress.rendered,
        expected_a_end_frame.expect("crossfade track A duration must be reported before EOF"),
    )
}

fn drive_app_layer_crossfade_advance(queue: &Queue, auto_advanced_index: &mut Option<usize>) {
    let crossfade_secs = f64::from(queue.crossfade_duration());
    if let (Some(pos), Some(dur)) = (queue.position_seconds(), queue.duration_seconds())
        && dur > crossfade_secs
        && pos >= dur - crossfade_secs
    {
        let current = queue.current_index().unwrap_or(0);
        if *auto_advanced_index != Some(current) && current + 1 < queue.len() {
            *auto_advanced_index = Some(current);
            let _ = queue.advance_to_next(Transition::Crossfade);
        }
    }
}

fn drain_variant_applied_events(
    events: &mut EventReceiver,
    committed_variant: &mut Option<usize>,
    record: bool,
) {
    loop {
        match events.try_recv().map(|env| env.event) {
            Ok(Event::Abr(AbrEvent::VariantApplied { to, .. })) => {
                let target = to.get();
                if record && (target == 1 || committed_variant.is_none()) {
                    *committed_variant = Some(target);
                }
            }
            Ok(_) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            Err(TryRecvError::Lagged(_)) => continue,
        }
    }
}

async fn render_seek_near_end_until_b_with_postroll(
    queue: &Queue,
    harness: &OfflinePlayerHarness,
    render_sample_rate: u32,
) -> (Vec<f32>, usize, f64) {
    let mut progress = RenderProgress::new();
    let mut seek_issue_frame: Option<usize> = None;
    let mut seek_duration: Option<f64> = None;

    for _ in 0..BLOCK_BUDGET {
        let _ = queue.tick();
        let block = harness.render(BLOCK_FRAMES);
        progress.push_block(&block, ASCENDING_TOL, seek_issue_frame);

        if seek_issue_frame.is_none()
            && progress.rendered_frames()
                >= usize::try_from(render_sample_rate).expect("render sample rate fits usize")
            && let Some(duration) = queue.duration_seconds()
            && duration > 7.0
        {
            queue.seek(duration - SEEK_OFFSET_SECS).expect("seek");
            seek_issue_frame = Some(progress.rendered_frames());
            seek_duration = Some(duration);
        }

        time::sleep(Duration::from_millis(1)).await;

        if progress.has_b_postroll() {
            break;
        }
    }

    (
        progress.rendered,
        seek_issue_frame.expect("seek must be issued after duration becomes available"),
        seek_duration.expect("duration must be recorded when seek is issued"),
    )
}

async fn render_until_tone_b_with_postroll(
    queue: &Queue,
    harness: &OfflinePlayerHarness,
    render_sample_rate: u32,
) -> (Vec<f32>, f64) {
    let mut progress = ToneRenderProgress::new();
    let mut track_duration: Option<f64> = None;

    for _ in 0..BLOCK_BUDGET {
        let _ = queue.tick();
        let block = harness.render(BLOCK_FRAMES);
        progress.push_block(&block, render_sample_rate);

        if track_duration.is_none()
            && let Some(duration) = queue.duration_seconds()
            && duration > 7.0
        {
            track_duration = Some(duration);
        }

        time::sleep(Duration::from_millis(1)).await;

        if progress.has_b_postroll() {
            break;
        }
    }

    (
        progress.rendered,
        track_duration.expect("track A duration must be reported before EOF"),
    )
}

impl RenderProgress {
    fn new() -> Self {
        Self {
            rendered: Vec::new(),
            left: Vec::new(),
            classes: Vec::new(),
            peak: 0.0,
            descending_seen_at: None,
        }
    }

    fn push_block(
        &mut self,
        block: &[f32],
        class_tolerance: f32,
        detect_descending_from: Option<usize>,
    ) {
        let block_left = deinterleave_left(block, usize::from(CHANNELS));
        self.peak = self.peak.max(max_abs(&block_left));

        let block_classes = if self.peak > MIN_RENDER_PEAK {
            let block_norm = normalize_with_gain(&block_left, self.peak);
            classify_windows(&block_norm, WINDOW_FRAMES, class_tolerance)
        } else {
            classify_windows(&block_left, WINDOW_FRAMES, class_tolerance)
        };
        self.classes.extend(block_classes);

        if self.descending_seen_at.is_none() {
            self.descending_seen_at = detect_descending_from.and_then(|min_frame| {
                first_sustained_class_window(&self.classes, FrameClass::Descending, min_frame)
                    .map(frame_for_window)
            });
        }

        self.left.extend_from_slice(&block_left);
        self.rendered.extend_from_slice(block);
    }

    fn rendered_frames(&self) -> usize {
        self.left.len()
    }

    fn has_b_postroll(&self) -> bool {
        self.descending_seen_at
            .is_some_and(|frame| self.rendered_frames() >= frame.saturating_add(POST_ROLL_FRAMES))
    }
}

struct ToneRenderProgress {
    rendered: Vec<f32>,
    left: Vec<f32>,
    classes: Vec<ToneClass>,
    b_seen_at: Option<usize>,
}

impl ToneRenderProgress {
    fn new() -> Self {
        Self {
            rendered: Vec::new(),
            left: Vec::new(),
            classes: Vec::new(),
            b_seen_at: None,
        }
    }

    fn push_block(&mut self, block: &[f32], sample_rate: u32) {
        let block_left = deinterleave_left(block, usize::from(CHANNELS));
        self.left.extend_from_slice(&block_left);
        self.rendered.extend_from_slice(block);

        while (self.classes.len() + 1) * TONE_WINDOW_FRAMES <= self.left.len() {
            let start = self.classes.len() * TONE_WINDOW_FRAMES;
            let end = start + TONE_WINDOW_FRAMES;
            self.classes
                .push(classify_tone_window(&self.left[start..end], sample_rate));
        }

        if self.b_seen_at.is_none() {
            self.b_seen_at = first_sustained_tone_window(&self.classes, ToneClass::B880, 0)
                .map(tone_frame_for_window);
        }
    }

    fn rendered_frames(&self) -> usize {
        self.left.len()
    }

    fn has_b_postroll(&self) -> bool {
        self.b_seen_at
            .is_some_and(|frame| self.rendered_frames() >= frame.saturating_add(POST_ROLL_FRAMES))
    }
}

fn find_seek_landing_frame(
    left: &[f32],
    seek_issue_frame: usize,
    b_onset_frame: usize,
    duration: f64,
) -> Option<usize> {
    let expected_phase =
        frames_from_secs(duration - SEEK_OFFSET_SECS, SAMPLE_RATE) % SAWTOOTH_PERIOD_FRAMES;
    let expected_phase = i32::try_from(expected_phase).expect("sawtooth phase fits i32");
    let latest_start = b_onset_frame.saturating_sub(SEEK_LANDING_WINDOW_FRAMES);

    (seek_issue_frame..=latest_start).find(|frame| {
        let phase_delta = phase_distance(phase_units(left[*frame]), expected_phase).abs();
        phase_delta <= SEEK_PHASE_TOL_UNITS
            && ascending_phase_replays(
                left,
                *frame,
                frame.saturating_add(SEEK_LANDING_WINDOW_FRAMES),
                PHASE_TOL_UNITS,
            )
            .is_empty()
    })
}

fn phase_distance(actual: i32, expected: i32) -> i32 {
    const SAWTOOTH_PERIOD_UNITS: i32 = 65_536;
    const SAWTOOTH_HALF_PERIOD_UNITS: i32 = 32_768;

    (actual - expected + SAWTOOTH_HALF_PERIOD_UNITS).rem_euclid(SAWTOOTH_PERIOD_UNITS)
        - SAWTOOTH_HALF_PERIOD_UNITS
}

fn deinterleave_left(samples: &[f32], channels: usize) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| frame[0])
        .collect()
}

fn normalized_left(left: &[f32], current_index: Option<usize>) -> Vec<f32> {
    let gain = max_abs(left);
    assert!(
        gain > MIN_RENDER_PEAK,
        "rendered signal peak too low for provenance analysis: gain_estimate={gain}; \
         current_index={current_index:?}"
    );
    normalize_with_gain(left, gain)
}

fn normalize_with_gain(left: &[f32], gain: f32) -> Vec<f32> {
    left.iter().map(|sample| *sample / gain).collect()
}

fn max_abs(samples: &[f32]) -> f32 {
    samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0, f32::max)
}

fn frames_from_secs(secs: f64, sample_rate: u32) -> usize {
    let secs = secs.max(0.0);
    if !secs.is_finite() {
        return usize::MAX;
    }

    let Ok(duration) = Duration::try_from_secs_f64(secs) else {
        return usize::MAX;
    };
    let sample_rate = u128::from(sample_rate);
    let whole_frames = u128::from(duration.as_secs()).saturating_mul(sample_rate);
    let fractional_frames = u128::from(duration.subsec_nanos())
        .saturating_mul(sample_rate)
        .saturating_add(500_000_000)
        / 1_000_000_000;

    usize::try_from(whole_frames.saturating_add(fractional_frames)).unwrap_or(usize::MAX)
}

fn classify_tone_windows(left: &[f32], window: usize, sample_rate: u32) -> Vec<ToneClass> {
    left.chunks_exact(window)
        .map(|samples| classify_tone_window(samples, sample_rate))
        .collect()
}

fn classify_tone_window(samples: &[f32], sample_rate: u32) -> ToneClass {
    let mag_a = goertzel_magnitude(
        samples,
        TONE_A_FREQ_HZ,
        usize::try_from(sample_rate).expect("sample rate fits usize"),
    );
    let mag_b = goertzel_magnitude(
        samples,
        TONE_B_FREQ_HZ,
        usize::try_from(sample_rate).expect("sample rate fits usize"),
    );
    let max_mag = mag_a.max(mag_b);
    if max_mag < TONE_MAG_FLOOR {
        ToneClass::Silence
    } else if mag_a > mag_b * 1.2 {
        ToneClass::A440
    } else if mag_b > mag_a * 1.2 {
        ToneClass::B880
    } else {
        ToneClass::Unknown
    }
}

fn goertzel_magnitude(samples: &[f32], freq_hz: f64, sample_rate: usize) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let omega = 2.0 * std::f64::consts::PI * freq_hz / sample_rate as f64;
    let coeff = 2.0 * omega.cos();
    let mut q1 = 0.0f64;
    let mut q2 = 0.0f64;
    for sample in samples {
        let q0 = coeff * q1 - q2 + f64::from(*sample);
        q2 = q1;
        q1 = q0;
    }
    let real = q1 - q2 * omega.cos();
    let imag = q2 * omega.sin();
    (real * real + imag * imag).sqrt()
}

fn require_first_non_silence(
    classes: &[FrameClass],
    runs: &[ClassRun],
    current_index: Option<usize>,
) -> usize {
    classes
        .iter()
        .position(|class| *class != FrameClass::Silence)
        .unwrap_or_else(|| {
            panic!(
                "audio onset must be rendered; {}",
                dump(&[], runs, None, None, current_index)
            )
        })
}

fn require_first_non_silence_with_switch(
    classes: &[FrameClass],
    runs: &[ClassRun],
    switch_issue_frame: usize,
    current_index: Option<usize>,
) -> usize {
    classes
        .iter()
        .position(|class| *class != FrameClass::Silence)
        .unwrap_or_else(|| {
            panic!(
                "audio onset must be rendered after late variant switch; {}",
                dump_with_switch(&[], runs, None, None, switch_issue_frame, current_index)
            )
        })
}

fn require_ascending_near_onset(
    classes: &[FrameClass],
    onset_window: usize,
    label: &str,
    diagnostics: &str,
) -> usize {
    let latest_allowed = onset_window.saturating_add(4);
    let region_end = latest_allowed.saturating_add(1).min(classes.len());
    let onset_region = classes.get(onset_window..region_end).unwrap_or(&[]);
    let first_ascending_window = classes
        .iter()
        .enumerate()
        .skip(onset_window)
        .find_map(|(idx, class)| (*class == FrameClass::Ascending).then_some(idx))
        .unwrap_or_else(|| {
            panic!(
                "{label}; no Ascending window found at or after onset; \
                 onset_window={onset_window}; latest_allowed={latest_allowed}; \
                 onset_region={onset_region:?}; {diagnostics}"
            )
        });

    assert!(
        first_ascending_window <= latest_allowed,
        "{label}; first_ascending_window={first_ascending_window}; \
         latest_allowed={latest_allowed}; onset_region={onset_region:?}; {diagnostics}"
    );
    first_ascending_window
}

fn require_first_tone_window(
    classes: &[ToneClass],
    target: ToneClass,
    min_frame: usize,
    runs: &[ToneRun],
    onset_window: Option<usize>,
    current_index: Option<usize>,
) -> usize {
    let start_window = min_frame / TONE_WINDOW_FRAMES;
    classes
        .iter()
        .enumerate()
        .skip(start_window)
        .find_map(|(idx, class)| (*class == target).then_some(idx))
        .unwrap_or_else(|| {
            panic!(
                "target tone class {target:?} must be rendered from frame {min_frame}; {}",
                tone_dump(runs, onset_window, None, current_index)
            )
        })
}

fn require_first_sustained_tone_window(
    classes: &[ToneClass],
    target: ToneClass,
    min_frame: usize,
    runs: &[ToneRun],
    onset_window: Option<usize>,
    current_index: Option<usize>,
) -> usize {
    first_sustained_tone_window(classes, target, min_frame).unwrap_or_else(|| {
        panic!(
            "sustained target tone class {target:?} must be rendered from frame {min_frame}; {}",
            tone_dump(runs, onset_window, None, current_index)
        )
    })
}

fn first_sustained_tone_window(
    classes: &[ToneClass],
    target: ToneClass,
    min_frame: usize,
) -> Option<usize> {
    classes
        .windows(SUSTAINED_DESCENDING_WINDOWS)
        .enumerate()
        .find_map(|(window, slice)| {
            let starts_after_gate = tone_frame_for_window(window) >= min_frame;
            (starts_after_gate && slice.iter().all(|class| *class == target)).then_some(window)
        })
}

fn require_first_sustained_descending_window(
    classes: &[FrameClass],
    min_frame: usize,
    runs: &[ClassRun],
    onset_window: Option<usize>,
    current_index: Option<usize>,
) -> usize {
    first_sustained_class_window(classes, FrameClass::Descending, min_frame).unwrap_or_else(|| {
        panic!(
            "sustained Descending B onset must be rendered from frame {min_frame}; {}",
            dump(&[], runs, onset_window, None, current_index)
        )
    })
}

fn require_first_sustained_descending_window_with_switch(
    classes: &[FrameClass],
    min_frame: usize,
    runs: &[ClassRun],
    onset_window: Option<usize>,
    switch_issue_frame: usize,
    current_index: Option<usize>,
) -> usize {
    first_sustained_class_window(classes, FrameClass::Descending, min_frame).unwrap_or_else(|| {
        panic!(
            "sustained Descending B onset must be rendered from frame {min_frame} \
             after late variant switch; {}",
            dump_with_switch(
                &[],
                runs,
                onset_window,
                None,
                switch_issue_frame,
                current_index,
            )
        )
    })
}

fn require_last_class_window_before(
    classes: &[FrameClass],
    target: FrameClass,
    before_window: usize,
    context: &ProvenanceDumpContext<'_>,
) -> usize {
    classes[..before_window]
        .iter()
        .rposition(|class| *class == target)
        .unwrap_or_else(|| {
            panic!(
                "target class {target:?} must appear before window {before_window}; {}",
                context.dump()
            )
        })
}

fn require_last_class_window_before_with_switch(
    classes: &[FrameClass],
    target: FrameClass,
    before_window: usize,
    context: &SwitchDumpContext<'_>,
) -> usize {
    classes[..before_window]
        .iter()
        .rposition(|class| *class == target)
        .unwrap_or_else(|| {
            panic!(
                "target class {target:?} must appear before window {before_window} \
                 after late variant switch; {}",
                context.dump()
            )
        })
}

fn first_sustained_class_window(
    classes: &[FrameClass],
    target: FrameClass,
    min_frame: usize,
) -> Option<usize> {
    classes
        .windows(SUSTAINED_DESCENDING_WINDOWS)
        .enumerate()
        .find_map(|(window, slice)| {
            let starts_after_gate = frame_for_window(window) >= min_frame;
            (starts_after_gate && slice.iter().all(|class| *class == target)).then_some(window)
        })
}

fn class_count_between(
    classes: &[FrameClass],
    target: FrameClass,
    start_window: usize,
    end_window: usize,
) -> usize {
    classes[start_window..end_window]
        .iter()
        .filter(|class| **class == target)
        .count()
}

fn assert_ascending_len(
    classes: &[FrameClass],
    expected_frames: usize,
    context: &ProvenanceDumpContext<'_>,
) {
    let onset_window = context.onset_window.expect("onset window must be set");
    let b_onset_window = context.b_onset_window.expect("B onset window must be set");
    let ascending_frames =
        class_count_between(classes, FrameClass::Ascending, onset_window, b_onset_window)
            * WINDOW_FRAMES;
    assert_close_len(
        ascending_frames,
        expected_frames,
        TRACK_FRAME_TOLERANCE,
        "track A ascending length must be exactly one full track before B starts",
        context,
    );
}

fn assert_ascending_len_with_switch(
    classes: &[FrameClass],
    expected_frames: usize,
    context: &SwitchDumpContext<'_>,
) {
    let onset_window = context.onset_window.expect("onset window must be set");
    let b_onset_window = context.b_onset_window.expect("B onset window must be set");
    let ascending_frames =
        class_count_between(classes, FrameClass::Ascending, onset_window, b_onset_window)
            * WINDOW_FRAMES;
    assert_close_len_with_switch(
        ascending_frames,
        expected_frames,
        TRACK_FRAME_TOLERANCE,
        "track A ascending length must be exactly one full track before B starts after variant switch",
        context,
    );
}

fn assert_close_len(
    actual: usize,
    expected: usize,
    tolerance: usize,
    label: &str,
    context: &ProvenanceDumpContext<'_>,
) {
    let lower = expected.saturating_sub(tolerance);
    let upper = expected.saturating_add(tolerance);
    assert!(
        actual >= lower && actual <= upper,
        "{label}: expected {expected} +/- {tolerance} frames, got {actual}; {}",
        context.dump()
    );
}

fn assert_close_len_with_switch(
    actual: usize,
    expected: usize,
    tolerance: usize,
    label: &str,
    context: &SwitchDumpContext<'_>,
) {
    let lower = expected.saturating_sub(tolerance);
    let upper = expected.saturating_add(tolerance);
    assert!(
        actual >= lower && actual <= upper,
        "{label}: expected {expected} +/- {tolerance} frames, got {actual}; {}",
        context.dump()
    );
}

fn assert_no_ascending_after_b(classes: &[FrameClass], context: &ProvenanceDumpContext<'_>) {
    let b_onset_window = context.b_onset_window.expect("B onset window must be set");
    let second_ascending = classes
        .iter()
        .enumerate()
        .skip(b_onset_window + 1)
        .find_map(|(idx, class)| (*class == FrameClass::Ascending).then_some(idx));
    assert!(
        second_ascending.is_none(),
        "B region must not contain Ascending A windows; second_ascending={second_ascending:?}; {}",
        context.dump()
    );
}

fn assert_no_ascending_after_b_with_switch(
    classes: &[FrameClass],
    context: &SwitchDumpContext<'_>,
) {
    let b_onset_window = context.b_onset_window.expect("B onset window must be set");
    let second_ascending = classes
        .iter()
        .enumerate()
        .skip(b_onset_window + 1)
        .find_map(|(idx, class)| (*class == FrameClass::Ascending).then_some(idx));
    assert!(
        second_ascending.is_none(),
        "B region must not contain Ascending A windows after late variant switch; \
         second_ascending={second_ascending:?}; {}",
        context.dump()
    );
}

fn assert_crossfade_contract(
    rendered: &[f32],
    queue: &Queue,
    expected_a_end_frame: usize,
    render_sample_rate: u32,
    collapse_runs: fn(&[ClassRun]) -> Vec<ClassRun>,
    label: &str,
) -> CrossfadeAnalysis {
    let left_raw = deinterleave_left(rendered, usize::from(CHANNELS));
    let left = normalized_left(&left_raw, queue.current_index());
    let classes = classify_windows(&left, WINDOW_FRAMES, ASCENDING_TOL);
    let raw_runs = class_runs(&classes);
    let collapsed_runs = collapse_runs(&raw_runs);
    let onset_window = require_first_non_silence(&classes, &raw_runs, queue.current_index());
    let b_onset_window = require_first_sustained_descending_window(
        &classes,
        0,
        &raw_runs,
        Some(onset_window),
        queue.current_index(),
    );
    let b_onset_frame = frame_for_window(b_onset_window);
    let render_rate = usize::try_from(render_sample_rate).expect("render sample rate fits usize");
    let analysis = CrossfadeAnalysis {
        raw_runs,
        collapsed_runs,
        onset_window,
        b_onset_window,
    };
    let context = analysis.context(expected_a_end_frame, queue.current_index());
    let earliest_b_onset = expected_a_end_frame.saturating_sub(6 * render_rate);
    let latest_b_onset = expected_a_end_frame.saturating_add(2 * render_rate);

    assert!(
        b_onset_frame >= earliest_b_onset && b_onset_frame <= latest_b_onset,
        "{label} B onset must land in the crossfade window; \
         b_onset_frame={b_onset_frame}; expected_range={earliest_b_onset}..={latest_b_onset}; {}",
        context.dump()
    );

    assert_no_late_sustained_ascending(&context, render_rate, label);

    assert_eq!(
        queue.current_index(),
        Some(1),
        "{label} queue.current_index must advance to track B; {}",
        context.dump()
    );

    analysis
}

fn assert_no_sustained_ascending_after_b_onset(context: &CrossfadeDumpContext<'_>, label: &str) {
    let b_onset_window = context.b_onset_window.expect("B onset window must be set");
    let sustained_ascending = context.collapsed_runs.iter().copied().find(|run| {
        run.0 == FrameClass::Ascending
            && run.2 >= SUSTAINED_ASCENDING_REPLAY_WINDOWS
            && run.1 > b_onset_window
    });

    assert!(
        sustained_ascending.is_none(),
        "{label} must not contain sustained Ascending A content after sustained Descending B onset; \
         sustained_ascending={sustained_ascending:?}; b_onset_window={b_onset_window}; {}",
        context.dump()
    );
}

fn assert_no_late_sustained_ascending(
    context: &CrossfadeDumpContext<'_>,
    render_rate: usize,
    label: &str,
) {
    let guard_frame = context.expected_a_end_frame.saturating_sub(render_rate / 4);
    let replay_run = context.collapsed_runs.iter().copied().find(|run| {
        run.0 == FrameClass::Ascending
            && run.2 >= SUSTAINED_ASCENDING_REPLAY_WINDOWS
            && frame_for_window(run.1) >= guard_frame
    });

    assert!(
        replay_run.is_none(),
        "{label} must not replay sustained Ascending A content near or after A end; \
         replay_run={replay_run:?}; guard_frame={guard_frame}; {}",
        context.dump()
    );
}

fn class_runs(classes: &[FrameClass]) -> Vec<ClassRun> {
    let Some((&first, rest)) = classes.split_first() else {
        return Vec::new();
    };
    let mut runs = Vec::new();
    let mut current = (first, 0, 1);

    for (idx, class) in rest.iter().copied().enumerate() {
        let window = idx + 1;
        if class == current.0 {
            current.2 += 1;
        } else {
            runs.push(current);
            current = (class, window, 1);
        }
    }

    runs.push(current);
    runs
}

fn tone_runs(classes: &[ToneClass]) -> Vec<ToneRun> {
    let Some((&first, rest)) = classes.split_first() else {
        return Vec::new();
    };
    let mut runs = Vec::new();
    let mut current = (first, 0, 1);

    for (idx, class) in rest.iter().copied().enumerate() {
        let window = idx + 1;
        if class == current.0 {
            current.2 += 1;
        } else {
            runs.push(current);
            current = (class, window, 1);
        }
    }

    runs.push(current);
    runs
}

/// Merges same-tone runs across silence underrun gaps.
///
/// Underrun gaps are unbounded under load, and silence carries no provenance,
/// so it should never fragment a tone region or contribute to its length.
fn merge_tone_underrun_gaps(runs: &[ToneRun]) -> Vec<ToneRun> {
    let mut merged_runs = Vec::new();
    let mut idx = 0;

    while idx < runs.len() {
        let run = runs[idx];
        if matches!(run.0, ToneClass::A440 | ToneClass::B880) {
            let mut merged_run = run;
            idx += 1;

            while idx + 1 < runs.len()
                && runs[idx].0 == ToneClass::Silence
                && runs[idx + 1].0 == run.0
            {
                merged_run.2 += runs[idx + 1].2;
                idx += 2;
            }

            merged_runs.push(merged_run);
        } else {
            merged_runs.push(run);
            idx += 1;
        }
    }

    merged_runs
}

fn collapse_short_unknown_islands(runs: &[ClassRun]) -> Vec<ClassRun> {
    collapse_noise_islands(runs, |_, class| class == FrameClass::Unknown)
}

fn collapse_resampled_noise_islands(runs: &[ClassRun]) -> Vec<ClassRun> {
    collapse_noise_islands(runs, is_resampled_noise_class)
}

fn collapse_noise_islands(
    runs: &[ClassRun],
    is_noise_class: fn(FrameClass, FrameClass) -> bool,
) -> Vec<ClassRun> {
    let mut collapsed: Vec<ClassRun> = Vec::new();
    let mut idx = 0;

    while idx < runs.len() {
        if let Some(last) = collapsed.last_mut()
            && is_noise_class(last.0, runs[idx].0)
        {
            let target = last.0;
            let mut end = idx;
            let mut noise_len = 0;

            while end < runs.len() && is_noise_class(target, runs[end].0) {
                noise_len += runs[end].2;
                end += 1;
            }

            if noise_len <= 4 && end < runs.len() && runs[end].0 == target {
                last.2 += noise_len + runs[end].2;
                idx = end + 1;
                continue;
            }
        }

        push_run(&mut collapsed, runs[idx]);
        idx += 1;
    }

    collapsed
}

fn is_resampled_noise_class(target: FrameClass, class: FrameClass) -> bool {
    match target {
        FrameClass::Ascending => matches!(class, FrameClass::Unknown | FrameClass::Descending),
        FrameClass::Descending => matches!(class, FrameClass::Unknown | FrameClass::Ascending),
        FrameClass::Silence | FrameClass::Unknown => false,
    }
}

fn push_run(runs: &mut Vec<ClassRun>, run: ClassRun) {
    if let Some(last) = runs.last_mut()
        && last.0 == run.0
    {
        last.2 += run.2;
        return;
    }
    runs.push(run);
}

fn no_audio_class_after_descending(runs: &[ClassRun], class: FrameClass) -> bool {
    let Some(descending_idx) = runs.iter().position(|run| run.0 == FrameClass::Descending) else {
        return false;
    };

    runs[descending_idx + 1..]
        .iter()
        .all(|run| matches!(run.0, FrameClass::Unknown | FrameClass::Silence) && run.0 != class)
}

fn no_tone_after_b(classes: &[ToneClass], b_onset_window: usize, class: ToneClass) -> bool {
    classes[b_onset_window..]
        .iter()
        .all(|tone_class| *tone_class != class)
}

fn dump(
    replays: &[Replay],
    runs: &[ClassRun],
    onset_window: Option<usize>,
    b_onset_window: Option<usize>,
    current_index: Option<usize>,
) -> String {
    format!(
        "replays={replays:?}; class_runs(class,start_window,len)={runs:?}; \
         onset_window={onset_window:?}; onset_frame={:?}; \
         b_onset_window={b_onset_window:?}; b_onset_frame={:?}; \
         current_index={current_index:?}",
        option_frame_for_window(onset_window),
        option_frame_for_window(b_onset_window)
    )
}

fn dump_with_switch(
    replays: &[Replay],
    runs: &[ClassRun],
    onset_window: Option<usize>,
    b_onset_window: Option<usize>,
    switch_issue_frame: usize,
    current_index: Option<usize>,
) -> String {
    format!(
        "replays={replays:?}; class_runs(class,start_window,len)={runs:?}; \
         onset_window={onset_window:?}; onset_frame={:?}; \
         b_onset_window={b_onset_window:?}; b_onset_frame={:?}; \
         switch_issue_frame={switch_issue_frame}; current_index={current_index:?}",
        option_frame_for_window(onset_window),
        option_frame_for_window(b_onset_window)
    )
}

fn tone_dump(
    runs: &[ToneRun],
    onset_window: Option<usize>,
    b_onset_window: Option<usize>,
    current_index: Option<usize>,
) -> String {
    format!(
        "tone_runs(class,start_window,len)={runs:?}; \
         onset_window={onset_window:?}; onset_frame={:?}; \
         b_onset_window={b_onset_window:?}; b_onset_frame={:?}; \
         current_index={current_index:?}",
        onset_window.map(tone_frame_for_window),
        b_onset_window.map(tone_frame_for_window)
    )
}

fn dump_with_collapsed(
    replays: &[Replay],
    raw_runs: &[ClassRun],
    collapsed_runs: &[ClassRun],
    onset_window: Option<usize>,
    b_onset_window: Option<usize>,
    current_index: Option<usize>,
) -> String {
    format!(
        "replays={replays:?}; raw_class_runs(class,start_window,len)={raw_runs:?}; \
         collapsed_class_runs(class,start_window,len)={collapsed_runs:?}; \
         onset_window={onset_window:?}; onset_frame={:?}; \
         b_onset_window={b_onset_window:?}; b_onset_frame={:?}; \
         current_index={current_index:?}",
        option_frame_for_window(onset_window),
        option_frame_for_window(b_onset_window)
    )
}

fn crossfade_dump(
    raw_runs: &[ClassRun],
    collapsed_runs: &[ClassRun],
    onset_window: Option<usize>,
    b_onset_window: Option<usize>,
    expected_a_end_frame: usize,
    current_index: Option<usize>,
) -> String {
    format!(
        "raw_class_runs(class,start_window,start_frame,len)={:?}; \
         collapsed_class_runs(class,start_window,start_frame,len)={:?}; \
         onset_window={onset_window:?}; onset_frame={:?}; \
         b_onset_window={b_onset_window:?}; b_onset_frame={:?}; \
         expected_a_end_frame={expected_a_end_frame}; current_index={current_index:?}",
        runs_with_frames(raw_runs),
        runs_with_frames(collapsed_runs),
        option_frame_for_window(onset_window),
        option_frame_for_window(b_onset_window)
    )
}

fn runs_with_frames(runs: &[ClassRun]) -> Vec<(FrameClass, usize, usize, usize)> {
    runs.iter()
        .map(|(class, start_window, len)| {
            (*class, *start_window, frame_for_window(*start_window), *len)
        })
        .collect()
}

fn frame_for_window(window: usize) -> usize {
    window * WINDOW_FRAMES
}

fn tone_frame_for_window(window: usize) -> usize {
    window * TONE_WINDOW_FRAMES
}

fn option_frame_for_window(window: Option<usize>) -> Option<usize> {
    window.map(frame_for_window)
}
