#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU128, NonZeroUsize};

use kithara::{
    platform::time::Duration,
    signal::{FrameCount, SessionFrame},
};
use kithara_integration_tests::{
    cochlea::{
        CochleaReport, continuity_failures, synchronization_failures, time_stretch_failures,
    },
    grid::Start,
    kithara,
    pcm_oracle::Audio,
};
use num_traits::AsPrimitive;

use super::sync_product_matrix::{
    Audible, BLOCK_FRAMES, CHANNELS, CUE, ONE_DECK, PreparedSources, ProductHarness,
    SHARED_DEADLINE, SHARED_DEADLINE_CONTROL, SyncCase, SyncRequest, mixed_sources, sweep_sources,
    synthetic_sources,
};

const TWENTY_MS_FRAMES: usize = 960;
const LATENCY_CALLBACK_FRAMES: usize = 128;

struct CommandRun {
    command_index: usize,
    command_lead: usize,
    source_anchor: Option<(u128, NonZeroU128)>,
    failures: Vec<String>,
    samples: Vec<f32>,
}

struct AlignedRun {
    candidate: Vec<f32>,
    command_index: usize,
    control: Vec<f32>,
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn tempo_retarget_run(
    driver_frames: usize,
    warm_blocks: usize,
    retarget: bool,
    prepared: &PreparedSources,
    wrong_source: Option<&Audio>,
) -> CommandRun {
    let response_budget =
        wrong_source.map(|_| NonZeroUsize::new(TWENTY_MS_FRAMES).expect("20 ms response budget"));
    let case = response_budget.map_or(ONE_DECK, |budget| ONE_DECK.response_budget(budget));
    let callback_frames = if wrong_source.is_some() {
        driver_frames.min(LATENCY_CALLBACK_FRAMES)
    } else {
        driver_frames
    };
    let mut harness =
        ProductHarness::new_for_block(case, prepared, CUE, Audible::Deck(0), callback_frames).await;
    harness.request_sync(case).await;
    let warm_frames = warm_blocks * BLOCK_FRAMES;
    for _ in 0..warm_frames.div_ceil(driver_frames) {
        hang_tick!(format!(
            "tempo retarget: warming {driver_frames}-frame blocks at frame {}",
            harness.host.position()
        ));
        let _ = harness.render(case, driver_frames).await;
        hang_reset!();
    }
    let source_anchor = Some(capture_source_anchor(&harness));
    let mut samples = if wrong_source.is_some() {
        harness
            .capture_source_frames(case, case.sample_rate as usize, driver_frames)
            .await
    } else {
        harness
            .capture_frames(case, case.sample_rate as usize, driver_frames)
            .await
    };
    if let Some(wrong) = wrong_source {
        let failures = harness
            .physical_failures(case, 0, &samples, wrong, None)
            .await;
        harness.failures.extend(failures);
    }
    let command_index = samples.len() / usize::from(CHANNELS);
    let command_lead = record_command_lead(&harness, "tempo", callback_frames, response_budget);
    let issued = SessionFrame::new(i64::try_from(harness.host.position()).expect("session frame"));
    let admitted = issued + FrameCount::new(command_lead);
    let first_speed = harness.decks[0].observe(|observation| observation.speeds.len());
    if retarget {
        harness.set_tempo(case, 132.0, false).await;
    }
    let after = if wrong_source.is_some() {
        harness
            .capture_source_frames(case, case.sample_rate as usize * 2, driver_frames)
            .await
    } else {
        harness
            .capture_frames(case, case.sample_rate as usize * 2, driver_frames)
            .await
    };
    if retarget && wrong_source.is_some() {
        let failures = harness
            .command_boundary_failures(issued, admitted, first_speed, None)
            .await;
        harness.failures.extend(failures);
    }
    if let Some(wrong) = wrong_source {
        let failures = harness
            .physical_failures(case, 0, &after, wrong, None)
            .await;
        harness.failures.extend(failures);
        let failures = harness.grid_binding_failures(case).await;
        harness.failures.extend(failures);
    }
    samples.extend(after);
    CommandRun {
        command_index,
        command_lead,
        source_anchor,
        failures: harness.failures,
        samples,
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn running_sync_run(
    driver_frames: usize,
    issue_sync: bool,
    prepared: &PreparedSources,
    wrong_source: &Audio,
) -> CommandRun {
    let response_budget = NonZeroUsize::new(TWENTY_MS_FRAMES).expect("20 ms response budget");
    let case = ONE_DECK.response_budget(response_budget);
    let callback_frames = driver_frames.min(LATENCY_CALLBACK_FRAMES);
    let mut harness =
        ProductHarness::new_for_block(case, prepared, CUE, Audible::Deck(0), callback_frames).await;
    let pre_frames = case.sample_rate as usize;
    let settled_frames = BLOCK_FRAMES * 96;
    let command_at_seconds = 8.0;
    let lead_frames: f64 = (settled_frames + pre_frames).as_();
    let seek_seconds = command_at_seconds - lead_frames / f64::from(case.sample_rate);
    let control = harness.decks[0].control().clone();
    hang_tick!(format!("running SYNC: seeking to {seek_seconds}s"));
    harness
        .host
        .run(move || control.seek(seek_seconds))
        .await
        .unwrap_or_else(|error| panic!("running SYNC fixture seek failed: {error}"));
    hang_reset!();
    harness
        .settle(case, settled_frames.div_ceil(callback_frames))
        .await;
    let source_anchor = Some(capture_source_anchor(&harness));
    let mut samples = harness
        .capture_source_frames(case, pre_frames, driver_frames)
        .await;
    let failures = harness
        .physical_failures(case, 0, &samples, wrong_source, None)
        .await;
    harness.failures.extend(failures);
    let command_index = samples.len() / usize::from(CHANNELS);
    let command_lead =
        record_command_lead(&harness, "SYNC", callback_frames, Some(response_budget));
    let issued = SessionFrame::new(i64::try_from(harness.host.position()).expect("session frame"));
    let admitted = issued + FrameCount::new(command_lead);
    let landing = admitted + FrameCount::new(240);
    let first_speed = harness.decks[0].observe(|observation| observation.speeds.len());
    if issue_sync {
        harness.request_sync(case).await;
    }
    let after = harness
        .capture_source_frames(case, pre_frames * 2, driver_frames)
        .await;
    let jump = issue_sync.then_some((admitted, landing));
    let failures = harness
        .physical_failures(case, 0, &after, wrong_source, jump)
        .await;
    harness.failures.extend(failures);
    if issue_sync {
        let failures = harness
            .command_boundary_failures(issued, admitted, first_speed, Some(landing))
            .await;
        harness.failures.extend(failures);
        let failures = harness.grid_binding_failures(case).await;
        harness.failures.extend(failures);
    }
    samples.extend(after);
    CommandRun {
        command_index,
        command_lead,
        source_anchor,
        failures: harness.failures,
        samples,
    }
}

fn record_command_lead(
    harness: &ProductHarness,
    label: &str,
    block_frames: usize,
    response_budget: Option<NonZeroUsize>,
) -> usize {
    const DEFAULT_PRELOAD_CHUNKS: usize = 3;
    const DEFAULT_RING_CHUNKS: usize = 10;

    let item = harness.decks[0].current().expect("selected PCM control").id;
    let snapshot = harness.decks[0]
        .track_snapshot(item)
        .expect("published PCM control");
    assert_eq!(harness.host.max_block_frames().get() as usize, block_frames);
    let quantum = block_frames.min(LATENCY_CALLBACK_FRAMES);
    let callback_chunks = block_frames.div_ceil(quantum);
    let ring_chunks = if response_budget.is_some() {
        callback_chunks + 1
    } else {
        (callback_chunks.max(DEFAULT_PRELOAD_CHUNKS) + 1).max(DEFAULT_RING_CHUNKS)
    };
    let ring = (ring_chunks + 1) * quantum - 1;
    assert_eq!(snapshot.track.ring_depth.get(), ring);
    assert_eq!(snapshot.track.engine_latency.get(), 0);
    assert_eq!(snapshot.track.declick.get(), 240);
    eprintln!(
        "PCM_COMMAND {label} block={block_frames} frame={} mark={:?} ring_depth={} engine_latency={} declick={}",
        harness.host.position(),
        snapshot.track.mark,
        snapshot.track.ring_depth.get(),
        snapshot.track.engine_latency.get(),
        snapshot.track.declick.get()
    );
    block_frames + ring
}

fn capture_source_anchor(harness: &ProductHarness) -> (u128, NonZeroU128) {
    harness.decks[0].observe(|observation| {
        let (_, slot) = observation
            .consumed
            .first()
            .expect("consumed source anchor");
        let mark = slot.mark.expect("consumed lane anchor");
        let info = slot.source.expect("consumed source payload");
        assert_eq!(mark.lane.segment, info.segment);
        let offset = mark
            .lane
            .frame
            .checked_sub(info.lane_frame)
            .expect("source packet offset");
        let (numerator, denominator) = info
            .source_span
            .expect("exact source span")
            .source_ratio_at(offset)
            .expect("mapped capture anchor");
        (numerator, denominator)
    })
}

fn wrong_source(prepared: &PreparedSources) -> Audio {
    kithara_integration_tests::pcm_oracle::prepared(&prepared.0.wrong_control_reference(0))
        .expect("independent wrong-source PCM")
}

fn align_source_runs(candidate: &CommandRun, control: &CommandRun) -> AlignedRun {
    let candidate_source = candidate.source_anchor.expect("candidate source anchor");
    let control_source = control.source_anchor.expect("control source anchor");
    let denominator = candidate_source
        .1
        .get()
        .checked_mul(control_source.1.get())
        .expect("alignment source denominator");
    let candidate_position = candidate_source
        .0
        .checked_mul(control_source.1.get())
        .expect("candidate source numerator");
    let control_position = control_source
        .0
        .checked_mul(candidate_source.1.get())
        .expect("control source numerator");
    let difference = candidate_position.abs_diff(control_position);
    assert_eq!(
        difference % denominator,
        0,
        "candidate and control must share the same fractional PCM phase",
    );
    let distance = usize::try_from(difference / denominator).expect("source alignment offset");
    let candidate_start = if candidate_position < control_position {
        distance
    } else {
        0
    };
    let control_start = if candidate_position > control_position {
        distance
    } else {
        0
    };
    assert!(candidate_start < candidate.command_index && control_start < control.command_index);
    aligned_slices(candidate, control, candidate_start, control_start)
}

fn command_latency_failures(
    label: &str,
    aligned: &AlignedRun,
    lead: usize,
    budget: usize,
) -> Vec<String> {
    let admitted = aligned.command_index + lead;
    let mut failures = Vec::new();
    if let Some(frame) = frame_deltas(&aligned.candidate, &aligned.control)
        .take(admitted)
        .position(|delta| delta != 0.0)
    {
        failures.push(format!(
            "{label}: PCM changed before the admitted lane frame at {frame}"
        ));
    }
    let transition = first_sustained_delta(
        &aligned.candidate,
        &aligned.control,
        admitted..aligned.candidate.len() / usize::from(CHANNELS),
    );
    match transition {
        Some(frame) => {
            let total = frame - aligned.command_index;
            let execution = frame - admitted;
            if total > TWENTY_MS_FRAMES {
                failures.push(format!("{label}: PCM changed after {total} frames from issue; budget is {TWENTY_MS_FRAMES}"));
            }
            if execution > budget {
                failures.push(format!("{label}: PCM changed after {execution} frames from admitted execution; budget is {budget}"));
            }
        }
        None => failures.push(format!("{label}: no sustained PCM change")),
    }
    failures
}

fn frame_deltas<'a>(candidate: &'a [f32], control: &'a [f32]) -> impl Iterator<Item = f32> + 'a {
    candidate
        .chunks_exact(usize::from(CHANNELS))
        .zip(control.chunks_exact(usize::from(CHANNELS)))
        .map(|(candidate, control)| {
            candidate
                .iter()
                .zip(control)
                .map(|(candidate, control)| (candidate - control).abs())
                .fold(0.0_f32, f32::max)
        })
}

fn align_runs(candidate: &CommandRun, control: &CommandRun) -> AlignedRun {
    const ALIGNMENT_FRAMES: usize = 4_096;
    const SAMPLE_STRIDE: usize = 8;

    let prefix = ALIGNMENT_FRAMES
        .min(candidate.command_index)
        .min(control.command_index);
    assert!(prefix > 0, "alignment needs pre-command PCM");
    let candidate_limit = candidate.command_index - prefix;
    let mut anchor = (f64::NEG_INFINITY, 0);
    for start in (0..=candidate_limit).step_by(SAMPLE_STRIDE) {
        let energy = (0..prefix)
            .step_by(SAMPLE_STRIDE)
            .map(|frame| {
                let sample = candidate.samples[(start + frame) * usize::from(CHANNELS)];
                let sample = f64::from(sample);
                sample * sample
            })
            .sum::<f64>();
        if energy >= anchor.0 {
            anchor = (energy, start);
        }
    }
    let candidate_anchor = anchor.1;
    let min_lag = -i64::try_from(candidate_anchor).expect("alignment anchor fits i64");
    let max_lag = i64::try_from(control.command_index - prefix).expect("command index fits i64")
        - i64::try_from(candidate_anchor).expect("alignment anchor fits i64");
    let mut best = (f64::INFINITY, i64::MAX, 0_i64);
    for lag in min_lag..=max_lag {
        let control_anchor = usize::try_from(
            i64::try_from(candidate_anchor).expect("alignment anchor fits i64") + lag,
        )
        .expect("control alignment anchor fits usize");
        let squared = (0..prefix)
            .step_by(SAMPLE_STRIDE)
            .map(|frame| {
                let candidate =
                    candidate.samples[(candidate_anchor + frame) * usize::from(CHANNELS)];
                let control = control.samples[(control_anchor + frame) * usize::from(CHANNELS)];
                let delta = f64::from(candidate - control);
                delta * delta
            })
            .sum::<f64>();
        if squared < best.0 || (squared == best.0 && lag.abs() < best.1) {
            best = (squared, lag.abs(), lag);
        }
    }
    let lag = best.2;
    let candidate_start = usize::try_from((-lag).max(0)).expect("alignment lag fits usize");
    let control_start = usize::try_from(lag.max(0)).expect("alignment lag fits usize");
    aligned_slices(candidate, control, candidate_start, control_start)
}

fn aligned_slices(
    candidate: &CommandRun,
    control: &CommandRun,
    candidate_start: usize,
    control_start: usize,
) -> AlignedRun {
    let candidate_frames = candidate.samples.len() / usize::from(CHANNELS) - candidate_start;
    let control_frames = control.samples.len() / usize::from(CHANNELS) - control_start;
    let frames = candidate_frames.min(control_frames);
    AlignedRun {
        candidate: candidate.samples[candidate_start * usize::from(CHANNELS)
            ..(candidate_start + frames) * usize::from(CHANNELS)]
            .to_vec(),
        command_index: candidate.command_index - candidate_start,
        control: control.samples[control_start * usize::from(CHANNELS)
            ..(control_start + frames) * usize::from(CHANNELS)]
            .to_vec(),
    }
}

fn first_sustained_delta(
    candidate: &[f32],
    control: &[f32],
    range: std::ops::Range<usize>,
) -> Option<usize> {
    const DELTA_THRESHOLD: f32 = 0.002;
    const SUSTAINED_FRAMES: usize = 32;

    let mut run = 0;
    for (frame, delta) in frame_deltas(candidate, control).enumerate() {
        if !range.contains(&frame) {
            continue;
        }
        if delta > DELTA_THRESHOLD {
            run += 1;
            if run == SUSTAINED_FRAMES {
                return Some(frame + 1 - SUSTAINED_FRAMES);
            }
        } else {
            run = 0;
        }
    }
    None
}

fn append_run_failures(label: &str, run: &CommandRun, failures: &mut Vec<String>) {
    failures.extend(
        run.failures
            .iter()
            .map(|failure| format!("{label}: {failure}")),
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
#[case::early_128("early", 47, 128)]
#[case::early_256("early", 47, 256)]
#[case::early_512("early", 47, 512)]
#[case::middle_128("middle", 94, 128)]
#[case::middle_256("middle", 94, 256)]
#[case::middle_512("middle", 94, 512)]
#[case::late_128("late", 140, 128)]
#[case::late_256("late", 140, 256)]
#[case::late_512("late", 140, 512)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn bound_tempo_retarget_reaches_pcm_within_twenty_ms(
    #[future(awt)] sweep_sources: PreparedSources,
    #[case] phase: &str,
    #[case] warm_blocks: usize,
    #[case] driver_frames: usize,
) {
    let wrong = wrong_source(&sweep_sources);
    hang_tick!(format!(
        "tempo retarget: {phase} phase, {driver_frames}-frame capture group"
    ));
    let control = tempo_retarget_run(
        driver_frames,
        warm_blocks,
        false,
        &sweep_sources,
        Some(&wrong),
    )
    .await;
    hang_reset!();
    let candidate = tempo_retarget_run(
        driver_frames,
        warm_blocks,
        true,
        &sweep_sources,
        Some(&wrong),
    )
    .await;
    hang_reset!();
    let aligned = align_source_runs(&candidate, &control);
    let control_report = CochleaReport::measure(&aligned.control, CHANNELS, 48_000);
    let candidate_report = CochleaReport::measure(&aligned.candidate, CHANNELS, 48_000);
    let mut failures =
        time_stretch_failures("bound tempo retarget", &candidate_report, &control_report);
    append_run_failures("control", &control, &mut failures);
    append_run_failures("candidate", &candidate, &mut failures);
    let latency_budget = TWENTY_MS_FRAMES.min(driver_frames * 2);
    failures.extend(command_latency_failures(
        "retarget",
        &aligned,
        candidate.command_lead,
        latency_budget,
    ));
    assert!(
        failures.is_empty(),
        "{driver_frames}-frame capture group/{LATENCY_CALLBACK_FRAMES}-frame callback {phase} retarget failed:\n{}",
        failures.join("\n"),
    );
}

#[cfg(feature = "flash")]
#[kithara::test(native, tokio, multi_thread, serial)]
async fn offline_callback_geometry_does_not_pace_the_audio_clock(
    #[future(awt)] synthetic_sources: PreparedSources,
) {
    let mut harness = ProductHarness::new_for_block(
        ONE_DECK,
        &synthetic_sources,
        CUE,
        Audible::Deck(0),
        BLOCK_FRAMES,
    )
    .await;
    let first = harness.host.position();
    let frames = ONE_DECK.sample_rate as usize;
    let started = kithara::platform::time::Instant::now();
    let pcm = harness.capture_frames(ONE_DECK, frames, 128).await;
    let elapsed = started.elapsed();
    assert_eq!(
        harness.host.position(),
        first + u64::from(ONE_DECK.sample_rate)
    );
    assert_eq!(pcm.len(), frames * usize::from(CHANNELS));
    assert!(pcm.iter().any(|sample| sample.abs() > 0.01));
    assert!(
        elapsed < Duration::from_secs(1),
        "offline PCM callbacks must not wait out their rendered audio duration: {elapsed:?}"
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn running_sync_command_changes_audible_pcm_within_one_block(
    #[future(awt)] synthetic_sources: PreparedSources,
) {
    let wrong = wrong_source(&synthetic_sources);
    for driver_frames in [128, 256, 512] {
        hang_tick!(format!(
            "running SYNC: {driver_frames}-frame capture group and candidate"
        ));
        let control = running_sync_run(driver_frames, false, &synthetic_sources, &wrong).await;
        hang_reset!();
        let candidate = running_sync_run(driver_frames, true, &synthetic_sources, &wrong).await;
        hang_reset!();
        let aligned = align_source_runs(&candidate, &control);
        let control_report = CochleaReport::measure(&aligned.control, CHANNELS, 48_000);
        let candidate_report = CochleaReport::measure(&aligned.candidate, CHANNELS, 48_000);
        let mut failures =
            time_stretch_failures("running SYNC", &candidate_report, &control_report);
        append_run_failures("control", &control, &mut failures);
        append_run_failures("candidate", &candidate, &mut failures);
        failures.extend(command_latency_failures(
            "running SYNC",
            &aligned,
            candidate.command_lead,
            driver_frames,
        ));
        assert!(
            failures.is_empty(),
            "running SYNC {driver_frames}-frame capture group/{LATENCY_CALLBACK_FRAMES}-frame callback contract failed:\n{}",
            failures.join("\n"),
        );
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn capture_intent_sequence(
    intents: &[SyncRequest],
    prepared: &PreparedSources,
) -> CommandRun {
    let mut harness = ProductHarness::new(ONE_DECK, prepared, CUE, Audible::Deck(0)).await;
    let control = harness.decks[0].control().clone();
    hang_tick!(format!("latest target: preparing {intents:?}"));
    harness
        .host
        .run(move || control.seek(5.25))
        .await
        .unwrap_or_else(|error| panic!("latest-target fixture seek failed: {error}"));
    hang_reset!();
    harness.settle(ONE_DECK, 96).await;
    for &intent in intents {
        hang_tick!(format!("capture_intent_sequence: submitting {intent:?}"));
        if matches!(intent, SyncRequest::Align) {
            harness.set_tempo(ONE_DECK, 132.0, false).await;
            hang_reset!();
        }
        harness.request_sync_intent(ONE_DECK, intent).await;
        hang_reset!();
    }
    let samples = harness
        .capture_frames(ONE_DECK, ONE_DECK.sample_rate as usize * 3, BLOCK_FRAMES)
        .await;
    CommandRun {
        command_index: 0,
        command_lead: 0,
        source_anchor: None,
        failures: harness.failures,
        samples,
    }
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn latest_sync_target_wins_in_pcm(#[future(awt)] synthetic_sources: PreparedSources) {
    let first = capture_intent_sequence(&[SyncRequest::On], &synthetic_sources).await;
    let second = capture_intent_sequence(&[SyncRequest::Off], &synthetic_sources).await;
    let latest = capture_intent_sequence(&[SyncRequest::Align], &synthetic_sources).await;
    let candidate = capture_intent_sequence(
        &[SyncRequest::On, SyncRequest::Off, SyncRequest::Align],
        &synthetic_sources,
    )
    .await;
    let latest_report = CochleaReport::measure(&latest.samples, CHANNELS, 48_000);
    let candidate_report = CochleaReport::measure(&candidate.samples, CHANNELS, 48_000);
    let mut failures = continuity_failures("latest target", &candidate_report, &latest_report);
    for (label, run) in [
        ("first", &first),
        ("second", &second),
        ("latest", &latest),
        ("candidate", &candidate),
    ] {
        append_run_failures(label, run, &mut failures);
    }
    if let Some(frame) = first_sustained_delta(
        &candidate.samples,
        &latest.samples,
        0..candidate.samples.len() / usize::from(CHANNELS),
    ) {
        failures.push(format!(
            "candidate diverged from the latest target at frame {frame}"
        ));
    }
    for (label, stale) in [("first", &first), ("second", &second)] {
        if first_sustained_delta(
            &candidate.samples,
            &stale.samples,
            0..candidate.samples.len() / usize::from(CHANNELS),
        )
        .is_none()
        {
            failures.push(format!(
                "candidate PCM is indistinguishable from stale {label} target"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "latest target PCM contract failed:\n{}",
        failures.join("\n"),
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(180)))]
async fn bound_sync_render_is_rtsan_clean(#[future(awt)] sweep_sources: PreparedSources) {
    let control = tempo_retarget_run(BLOCK_FRAMES, 16, false, &sweep_sources, None).await;
    let candidate = tempo_retarget_run(BLOCK_FRAMES, 16, true, &sweep_sources, None).await;
    let aligned = align_runs(&candidate, &control);
    let control_report = CochleaReport::measure(&aligned.control, CHANNELS, 48_000);
    let candidate_report = CochleaReport::measure(&aligned.candidate, CHANNELS, 48_000);
    let mut failures =
        time_stretch_failures("RTSan bound render", &candidate_report, &control_report);
    append_run_failures("control", &control, &mut failures);
    append_run_failures("candidate", &candidate, &mut failures);
    assert!(
        failures.is_empty(),
        "bound RTSan PCM contract failed:\n{}",
        failures.join("\n"),
    );
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn shared_worker_capture(
    case: SyncCase,
    prepared: &PreparedSources,
    wrong_source: &Audio,
) -> CommandRun {
    // Keep all four short sources playing through the complete six-beat capture.
    let start = Start::Seconds(1.25);
    let mut harness = ProductHarness::new(case, prepared, start, Audible::Deck(0)).await;
    let operations_frame = u64::from(case.sample_rate) * 2;
    while harness.host.position() < operations_frame {
        hang_tick!(format!(
            "{}: advancing setup frame {} to {operations_frame}",
            case.id(),
            harness.host.position()
        ));
        let remaining = usize::try_from(operations_frame - harness.host.position())
            .expect("fixture setup frame distance fits usize");
        let _ = harness.render(case, remaining.min(BLOCK_FRAMES)).await;
        hang_reset!();
    }
    harness.run_operations(case).await;
    harness.ride_tempo(case).await;
    if harness
        .decks
        .iter()
        .any(|deck| !deck.engine_load().is_active())
    {
        harness
            .failures
            .push("shared worker did not report active decode load".to_owned());
    }
    let frames: usize = (f64::from(case.sample_rate) * 60.0 / case.final_bpm() * 6.0)
        .round()
        .as_();
    let samples = harness.capture_paced(case, frames).await;
    assert!(
        harness
            .decks
            .iter()
            .all(|deck| deck.engine_load().is_active()),
        "shared worker decode load must remain active through the final captured frame"
    );
    match harness
        .physical_reference(case, 0, &samples, wrong_source, None)
        .await
    {
        Ok(expected) => {
            assert_eq!(
                samples.len(),
                expected.len(),
                "full physical capture length"
            );
            let actual = CochleaReport::measure(&samples, CHANNELS, case.sample_rate);
            let reference = CochleaReport::measure(&expected, CHANNELS, case.sample_rate);
            harness
                .failures
                .extend(time_stretch_failures(case.id(), &actual, &reference));
        }
        Err(error) => harness.failures.push(error),
    }
    let failures = harness.grid_binding_failures(case).await;
    harness.failures.extend(failures);
    let failures = harness.grid_anchor_failures(case, None).await;
    harness.failures.extend(failures);
    CommandRun {
        command_index: 0,
        command_lead: 0,
        source_anchor: None,
        failures: harness.failures,
        samples,
    }
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn bound_sync_pcm_stays_clean_under_shared_worker_deadline_load(
    #[future(awt)] mixed_sources: PreparedSources,
) {
    let wrong = wrong_source(&mixed_sources);
    let control = shared_worker_capture(SHARED_DEADLINE_CONTROL, &mixed_sources, &wrong).await;
    let candidate = shared_worker_capture(SHARED_DEADLINE, &mixed_sources, &wrong).await;
    let mut failures = Vec::new();
    append_run_failures("control", &control, &mut failures);
    append_run_failures("candidate", &candidate, &mut failures);
    if !failures.is_empty() {
        let supplemental = synchronization_failures(
            "bound shared-worker load",
            &[candidate.samples.as_slice()],
            CHANNELS,
            48_000,
            SHARED_DEADLINE.final_bpm(),
        );
        eprintln!("ORACLE_SUPPLEMENTAL_PEAKS bound shared-worker load: {supplemental:?}");
    }
    assert!(
        failures.is_empty(),
        "bound shared-worker deadline contract failed:\n{}",
        failures.join("\n"),
    );
}
