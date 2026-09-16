#![cfg(not(target_arch = "wasm32"))]

use kithara::{platform::time::Duration, warp::SyncIntent};
use kithara_integration_tests::{
    cochlea::{
        CochleaReport, continuity_failures, marked_synchronization_failures, time_stretch_failures,
    },
    kithara, usdt_trace,
};

use super::sync_product_matrix::{
    BLOCK_FRAMES, CHANNELS, ONE_DECK, PreparedSources, ProductHarness, SHARED_DEADLINE,
    SHARED_DEADLINE_CONTROL, SyncCase, mixed_sources, sweep_sources, synthetic_sources,
};

const TWENTY_MS_FRAMES: usize = 960;

/// Control-to-audible ceiling at a 128-frame output block.
const RESPONSE_CEILING_FRAMES: usize = 448;

struct CommandRun {
    activation_index: Option<usize>,
    command_index: usize,
    failures: Vec<String>,
    samples: Vec<f32>,
}

struct AlignedRun {
    activation_index: Option<usize>,
    candidate: Vec<f32>,
    command_index: usize,
    control: Vec<f32>,
}

async fn tempo_retarget_run(
    block_frames: usize,
    warm_blocks: usize,
    retarget: bool,
    prepared: &PreparedSources,
) -> CommandRun {
    let mut harness = ProductHarness::new_for_block(ONE_DECK, prepared, 0, block_frames).await;
    harness.request_sync(ONE_DECK).await;
    harness.settle_sync_activation(ONE_DECK).await;
    let warm_frames = warm_blocks * BLOCK_FRAMES;
    for _ in 0..warm_frames.div_ceil(block_frames) {
        let _ = harness.render(ONE_DECK, block_frames).await;
    }
    let mut samples = harness
        .capture_frames(ONE_DECK, ONE_DECK.sample_rate as usize, block_frames)
        .await;
    let command_index = samples.len() / usize::from(CHANNELS);
    let command_output = harness.rendered_frames;
    let trace = retarget.then(usdt_trace::scope);
    if retarget {
        harness.set_tempo(ONE_DECK, 132.0, false).await;
    }
    samples.extend(
        harness
            .capture_frames(ONE_DECK, ONE_DECK.sample_rate as usize * 2, block_frames)
            .await,
    );
    let activation_index = trace.and_then(|trace| {
        let capture_start = command_output.checked_sub(u64::try_from(command_index).ok()?)?;
        trace
            .events()
            .iter()
            .filter(|event| event.probe == "publish")
            .filter(|event| {
                event
                    .field("transport_revision")
                    .is_some_and(|revision| revision > 1)
            })
            .filter_map(|event| event.field("output_start"))
            .min()
            .and_then(|output| output.checked_sub(capture_start))
            .and_then(|frames| usize::try_from(frames).ok())
    });
    let underruns = harness.underrun_failures();
    let mut failures = harness.failures;
    failures.extend(underruns);
    CommandRun {
        activation_index,
        command_index,
        failures,
        samples,
    }
}

async fn running_sync_run(
    block_frames: usize,
    issue_sync: bool,
    prepared: &PreparedSources,
) -> CommandRun {
    let mut harness = ProductHarness::new_for_block(ONE_DECK, prepared, 0, block_frames).await;
    let pre_frames = ONE_DECK.sample_rate as usize;
    let settled_frames = BLOCK_FRAMES * 96;
    let command_at_seconds = 8.0;
    let seek_seconds =
        command_at_seconds - (settled_frames + pre_frames) as f64 / f64::from(ONE_DECK.sample_rate);
    harness.decks[0]
        .seek(seek_seconds)
        .unwrap_or_else(|error| panic!("running SYNC fixture seek failed: {error}"));
    harness
        .settle(ONE_DECK, settled_frames.div_ceil(block_frames))
        .await;
    let mut samples = harness
        .capture_frames(ONE_DECK, pre_frames, block_frames)
        .await;
    let command_index = samples.len() / usize::from(CHANNELS);
    let command_output = harness.rendered_frames;
    let trace = issue_sync.then(usdt_trace::scope);
    if issue_sync {
        harness.request_sync(ONE_DECK).await;
    }
    samples.extend(
        harness
            .capture_frames(ONE_DECK, block_frames * 2, block_frames)
            .await,
    );
    let activation_index = trace.as_ref().and_then(|trace| {
        let capture_start = command_output.checked_sub(u64::try_from(command_index).ok()?)?;
        trace
            .events()
            .iter()
            .filter(|event| event.probe == "warp_plan_published")
            .filter_map(|event| event.field("activation_output"))
            .min()
            .and_then(|output| output.checked_sub(capture_start))
            .and_then(|frames| usize::try_from(frames).ok())
    });
    samples.extend(
        harness
            .capture_frames(ONE_DECK, pre_frames * 4 - block_frames * 2, block_frames)
            .await,
    );
    let underruns = harness.underrun_failures();
    let mut failures = harness.failures;
    failures.extend(underruns);
    CommandRun {
        activation_index,
        command_index,
        failures,
        samples,
    }
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
    let candidate_anchor = candidate.command_index - prefix;
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
    let candidate_frames = candidate.samples.len() / usize::from(CHANNELS) - candidate_start;
    let control_frames = control.samples.len() / usize::from(CHANNELS) - control_start;
    let frames = candidate_frames.min(control_frames);
    AlignedRun {
        activation_index: candidate
            .activation_index
            .and_then(|index| index.checked_sub(candidate_start)),
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
async fn bound_tempo_retarget_reaches_pcm_within_twenty_ms(
    #[future(awt)] sweep_sources: PreparedSources,
) {
    for (phase, warm_blocks) in [("early", 47), ("middle", 94), ("late", 140)] {
        for block_frames in [128, 256, 512] {
            let control =
                tempo_retarget_run(block_frames, warm_blocks, false, &sweep_sources).await;
            let candidate =
                tempo_retarget_run(block_frames, warm_blocks, true, &sweep_sources).await;
            let aligned = align_runs(&candidate, &control);
            let control_report = CochleaReport::measure(&aligned.control, CHANNELS, 48_000);
            let candidate_report = CochleaReport::measure(&aligned.candidate, CHANNELS, 48_000);
            let mut failures =
                time_stretch_failures("bound tempo retarget", &candidate_report, &control_report);
            append_run_failures("control", &control, &mut failures);
            append_run_failures("candidate", &candidate, &mut failures);
            if let Some(frame) = first_sustained_delta(
                &aligned.candidate,
                &aligned.control,
                0..aligned.command_index,
            ) {
                failures.push(format!(
                    "candidate diverged before retarget at frame {frame}"
                ));
            }
            let transition = first_sustained_delta(
                &aligned.candidate,
                &aligned.control,
                aligned
                    .activation_index
                    .expect("tempo retarget publishes its transport activation")
                    ..aligned.candidate.len() / usize::from(CHANNELS),
            );
            let activation_index = aligned
                .activation_index
                .expect("tempo retarget publishes its transport activation");
            let quantization_wait = activation_index - aligned.command_index;
            if quantization_wait > block_frames {
                failures.push(format!(
                    "retarget activation waited {quantization_wait} frames; budget is {block_frames}"
                ));
            }
            let latency_budget = TWENTY_MS_FRAMES
                .min(block_frames * 2)
                .min(RESPONSE_CEILING_FRAMES + block_frames - 128);
            match transition.map(|frame| frame - activation_index) {
                Some(frames) if frames <= latency_budget => {}
                Some(frames) => failures.push(format!(
                    "retarget changed PCM after {frames} frames; budget is {latency_budget}"
                )),
                None => failures.push("retarget produced no sustained PCM change".to_owned()),
            }
            assert!(
                failures.is_empty(),
                "{block_frames}-frame {phase} retarget failed:\n{}",
                failures.join("\n"),
            );
        }
    }
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn running_sync_command_changes_audible_pcm_at_planned_activation(
    #[future(awt)] synthetic_sources: PreparedSources,
) {
    for block_frames in [128, 256, 512] {
        let control = running_sync_run(block_frames, false, &synthetic_sources).await;
        let candidate = running_sync_run(block_frames, true, &synthetic_sources).await;
        let aligned = align_runs(&candidate, &control);
        let control_report = CochleaReport::measure(&aligned.control, CHANNELS, 48_000);
        let candidate_report = CochleaReport::measure(&aligned.candidate, CHANNELS, 48_000);
        let mut failures =
            time_stretch_failures("running SYNC", &candidate_report, &control_report);
        append_run_failures("control", &control, &mut failures);
        append_run_failures("candidate", &candidate, &mut failures);
        if let Some(frame) = first_sustained_delta(
            &aligned.candidate,
            &aligned.control,
            0..aligned.command_index,
        ) {
            failures.push(format!("candidate diverged before SYNC at frame {frame}"));
        }
        let activation_index = aligned
            .activation_index
            .expect("running SYNC publishes its planned activation");
        if let Some(frame) = first_sustained_delta(
            &aligned.candidate,
            &aligned.control,
            aligned.command_index..activation_index,
        ) {
            let best_lag = (-256_i64..=256)
                .filter_map(|lag| {
                    let control_start = usize::try_from(i64::try_from(frame).ok()? + lag).ok()?;
                    let span = 512.min(
                        aligned
                            .control
                            .len()
                            .checked_div(usize::from(CHANNELS))?
                            .checked_sub(control_start)?,
                    );
                    let error = (0..span)
                        .map(|offset| {
                            let candidate =
                                aligned.candidate[(frame + offset) * usize::from(CHANNELS)];
                            let control =
                                aligned.control[(control_start + offset) * usize::from(CHANNELS)];
                            f64::from((candidate - control).abs())
                        })
                        .sum::<f64>();
                    Some((error, lag))
                })
                .min_by(|left, right| left.0.total_cmp(&right.0));
            eprintln!(
                "SYNC_DELTA frame={frame} command={} activation={} best_lag={best_lag:?} candidate={:?} control={:?}",
                aligned.command_index,
                activation_index,
                &aligned.candidate
                    [frame * usize::from(CHANNELS)..(frame + 4) * usize::from(CHANNELS)],
                &aligned.control
                    [frame * usize::from(CHANNELS)..(frame + 4) * usize::from(CHANNELS)],
            );
            failures.push(format!(
                "running SYNC changed PCM {frames} frames before planned activation",
                frames = activation_index - frame,
            ));
        }
        let transition = first_sustained_delta(
            &aligned.candidate,
            &aligned.control,
            activation_index..aligned.candidate.len() / usize::from(CHANNELS),
        );
        match transition.map(|frame| frame - activation_index) {
            Some(frames) if frames <= 40 => {}
            Some(frames) => failures.push(format!(
                "running SYNC changed PCM {frames} frames after activation; blend budget is 40"
            )),
            None => failures
                .push("running SYNC produced no sustained PCM change at activation".to_owned()),
        }
        assert!(
            failures.is_empty(),
            "running SYNC {block_frames}-frame contract failed:\n{}",
            failures.join("\n"),
        );
    }
}

async fn capture_intent_sequence(intents: &[SyncIntent], prepared: &PreparedSources) -> CommandRun {
    let mut harness = ProductHarness::new(ONE_DECK, prepared, 0).await;
    harness.decks[0]
        .seek(5.25)
        .unwrap_or_else(|error| panic!("latest-target fixture seek failed: {error}"));
    harness.settle(ONE_DECK, 96).await;
    for &intent in intents {
        harness.request_sync_intent(ONE_DECK, intent).await;
    }
    let samples = harness
        .capture_frames(ONE_DECK, ONE_DECK.sample_rate as usize * 3, BLOCK_FRAMES)
        .await;
    CommandRun {
        activation_index: None,
        command_index: 0,
        failures: harness.failures,
        samples,
    }
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn latest_sync_target_wins_in_pcm(#[future(awt)] synthetic_sources: PreparedSources) {
    let first = capture_intent_sequence(&[SyncIntent::Enable], &synthetic_sources).await;
    let second = capture_intent_sequence(&[SyncIntent::Disable], &synthetic_sources).await;
    let latest = capture_intent_sequence(&[SyncIntent::AlignNow], &synthetic_sources).await;
    let candidate = capture_intent_sequence(
        &[
            SyncIntent::Enable,
            SyncIntent::Disable,
            SyncIntent::AlignNow,
        ],
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
    let control = tempo_retarget_run(BLOCK_FRAMES, 16, false, &sweep_sources).await;
    let candidate = tempo_retarget_run(BLOCK_FRAMES, 16, true, &sweep_sources).await;
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

async fn shared_worker_capture(case: SyncCase, prepared: &PreparedSources) -> CommandRun {
    let mut harness = ProductHarness::new_for_block(case, prepared, 0, BLOCK_FRAMES).await;
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
    let underruns_before = harness
        .player_controls
        .iter()
        .map(|control| control.rt_metrics().map(|metrics| metrics.underruns()))
        .collect::<Option<Vec<_>>>();
    let frames = (f64::from(case.sample_rate) * 60.0 / case.final_bpm() * 6.0).round() as usize;
    let samples = harness.capture_paced(case, frames).await;
    let engine_loads = harness
        .decks
        .iter()
        .map(|deck| deck.engine_load())
        .collect::<Vec<_>>();
    let underruns_after = harness
        .player_controls
        .iter()
        .map(|control| control.rt_metrics().map(|metrics| metrics.underruns()))
        .collect::<Option<Vec<_>>>();
    match (underruns_before, underruns_after) {
        (Some(before), Some(after)) if before == after => {}
        (Some(before), Some(after)) => harness.failures.push(format!(
            "shared-worker capture incremented RT underruns: before={before:?}, after={after:?}, engine_loads={engine_loads:?}"
        )),
        _ => harness
            .failures
            .push("shared-worker capture did not expose RT metrics".to_owned()),
    }
    let command_index = samples.len() / usize::from(CHANNELS);
    CommandRun {
        activation_index: None,
        command_index,
        failures: harness.failures,
        samples,
    }
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn bound_sync_pcm_stays_clean_under_shared_worker_deadline_load(
    #[future(awt)] mixed_sources: PreparedSources,
) {
    let control = shared_worker_capture(SHARED_DEADLINE_CONTROL, &mixed_sources).await;
    let candidate = shared_worker_capture(SHARED_DEADLINE, &mixed_sources).await;
    let aligned = align_runs(&candidate, &control);
    let control_report = CochleaReport::measure(&aligned.control, CHANNELS, 48_000);
    let candidate_report = CochleaReport::measure(&aligned.candidate, CHANNELS, 48_000);
    let mut failures = time_stretch_failures(
        "bound shared-worker load",
        &candidate_report,
        &control_report,
    );
    failures.extend(marked_synchronization_failures(
        "bound shared-worker load",
        &[aligned.candidate.as_slice()],
        CHANNELS,
        48_000,
        SHARED_DEADLINE.final_bpm(),
    ));
    append_run_failures("control", &control, &mut failures);
    append_run_failures("candidate", &candidate, &mut failures);
    assert!(
        failures.is_empty(),
        "bound shared-worker deadline contract failed:\n{}",
        failures.join("\n"),
    );
}
