#![cfg(not(target_arch = "wasm32"))]

use kithara::{platform::time::Duration, warp::SyncIntent};
use kithara_integration_tests::{
    cochlea::{
        AlignedPcm, CochleaReport, align_command_runs, continuity_failures,
        first_sustained_delta as sustained_delta, synchronization_failures, time_stretch_failures,
    },
    kithara,
};

use super::sync_product_matrix::{
    BLOCK_FRAMES, CHANNELS, HlsProtection, ONE_DECK, ProductHarness, Provider, SHARED_DEADLINE,
    SHARED_DEADLINE_CONTROL, SyncCase,
};

const TWENTY_MS_FRAMES: usize = 960;

struct CommandRun {
    command_index: usize,
    failures: Vec<String>,
    samples: Vec<f32>,
}

async fn tempo_retarget_run(block_frames: usize, warm_blocks: usize, retarget: bool) -> CommandRun {
    let mut harness =
        ProductHarness::new_for_block(ONE_DECK, Provider::Sweep, 0, block_frames).await;
    harness.request_sync(ONE_DECK).await;
    let warm_frames = warm_blocks * BLOCK_FRAMES;
    for _ in 0..warm_frames.div_ceil(block_frames) {
        let _ = harness.render(ONE_DECK, block_frames).await;
    }
    let mut samples = harness
        .capture_frames(ONE_DECK, ONE_DECK.sample_rate as usize, block_frames)
        .await;
    let command_index = samples.len() / usize::from(CHANNELS);
    if retarget {
        harness.set_tempo(ONE_DECK, 132.0, false);
    }
    samples.extend(
        harness
            .capture_frames(ONE_DECK, ONE_DECK.sample_rate as usize * 2, block_frames)
            .await,
    );
    CommandRun {
        command_index,
        failures: harness.failures,
        samples,
    }
}

async fn running_sync_run(block_frames: usize, issue_sync: bool) -> CommandRun {
    let mut harness =
        ProductHarness::new_for_block(ONE_DECK, Provider::Synthetic, 0, block_frames).await;
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
    if issue_sync {
        harness.request_sync(ONE_DECK).await;
    }
    samples.extend(
        harness
            .capture_frames(ONE_DECK, pre_frames * 2, block_frames)
            .await,
    );
    CommandRun {
        command_index,
        failures: harness.failures,
        samples,
    }
}

fn align_runs(candidate: &CommandRun, control: &CommandRun) -> AlignedPcm {
    align_command_runs(
        &candidate.samples,
        candidate.command_index,
        &control.samples,
        control.command_index,
        CHANNELS,
    )
}

fn first_sustained_delta(
    candidate: &[f32],
    control: &[f32],
    range: std::ops::Range<usize>,
) -> Option<usize> {
    sustained_delta(candidate, control, CHANNELS, range, 0.002, 32)
}

fn append_run_failures(label: &str, run: &CommandRun, failures: &mut Vec<String>) {
    failures.extend(
        run.failures
            .iter()
            .map(|failure| format!("{label}: {failure}")),
    );
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[ignore = "ignored-red: bound Warp tempo retarget is not implemented"]
async fn bound_tempo_retarget_reaches_pcm_within_twenty_ms() {
    for (phase, warm_blocks) in [("early", 47), ("middle", 94), ("late", 140)] {
        for block_frames in [128, 256, 512] {
            let control = tempo_retarget_run(block_frames, warm_blocks, false).await;
            let candidate = tempo_retarget_run(block_frames, warm_blocks, true).await;
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
                0..aligned.command_frame,
            ) {
                failures.push(format!(
                    "candidate diverged before retarget at frame {frame}"
                ));
            }
            let transition = first_sustained_delta(
                &aligned.candidate,
                &aligned.control,
                aligned.command_frame..aligned.candidate.len() / usize::from(CHANNELS),
            );
            let latency_budget = TWENTY_MS_FRAMES.min(block_frames * 2);
            match transition.map(|frame| frame - aligned.command_frame) {
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

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[ignore = "ignored-red: running Warp alignment is not implemented"]
async fn running_sync_command_changes_audible_pcm_within_one_block() {
    for block_frames in [128, 256, 512] {
        let control = running_sync_run(block_frames, false).await;
        let candidate = running_sync_run(block_frames, true).await;
        let aligned = align_runs(&candidate, &control);
        let control_report = CochleaReport::measure(&aligned.control, CHANNELS, 48_000);
        let candidate_report = CochleaReport::measure(&aligned.candidate, CHANNELS, 48_000);
        let mut failures = continuity_failures("running SYNC", &candidate_report, &control_report);
        append_run_failures("control", &control, &mut failures);
        append_run_failures("candidate", &candidate, &mut failures);
        if let Some(frame) = first_sustained_delta(
            &aligned.candidate,
            &aligned.control,
            0..aligned.command_frame,
        ) {
            failures.push(format!("candidate diverged before SYNC at frame {frame}"));
        }
        let transition = first_sustained_delta(
            &aligned.candidate,
            &aligned.control,
            aligned.command_frame..aligned.candidate.len() / usize::from(CHANNELS),
        );
        match transition.map(|frame| frame - aligned.command_frame) {
            Some(frames) if frames <= block_frames => {}
            Some(frames) => failures.push(format!(
                "running SYNC changed PCM after {frames} frames; budget is {block_frames}"
            )),
            None => failures.push("running SYNC produced no sustained PCM change".to_owned()),
        }
        assert!(
            failures.is_empty(),
            "running SYNC {block_frames}-frame contract failed:\n{}",
            failures.join("\n"),
        );
    }
}

async fn capture_intent_sequence(intents: &[SyncIntent]) -> CommandRun {
    let mut harness = ProductHarness::new(ONE_DECK, Provider::Synthetic, 0).await;
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
        command_index: 0,
        failures: harness.failures,
        samples,
    }
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[ignore = "ignored-red: latest Warp target replacement is not implemented"]
async fn latest_sync_target_wins_in_pcm() {
    let first = capture_intent_sequence(&[SyncIntent::Enable]).await;
    let second = capture_intent_sequence(&[SyncIntent::Disable]).await;
    let latest = capture_intent_sequence(&[SyncIntent::AlignNow]).await;
    let candidate = capture_intent_sequence(&[
        SyncIntent::Enable,
        SyncIntent::Disable,
        SyncIntent::AlignNow,
    ])
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

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(180))
)]
#[ignore = "ignored-red: bound Warp render is not implemented"]
async fn bound_sync_render_is_rtsan_clean() {
    let control = tempo_retarget_run(BLOCK_FRAMES, 16, false).await;
    let candidate = tempo_retarget_run(BLOCK_FRAMES, 16, true).await;
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

async fn shared_worker_capture(case: SyncCase) -> CommandRun {
    let mut harness = ProductHarness::new(case, Provider::HlsMp3(HlsProtection::Plain), 0).await;
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
    let frames = (f64::from(case.sample_rate) * 60.0 / case.final_bpm() * 6.0).round() as usize;
    let samples = harness.capture_paced(case, frames).await;
    CommandRun {
        command_index: 0,
        failures: harness.failures,
        samples,
    }
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[ignore = "ignored-red: bound Warp shared-worker path is not implemented"]
async fn bound_sync_pcm_stays_clean_under_shared_worker_deadline_load() {
    let control = shared_worker_capture(SHARED_DEADLINE_CONTROL).await;
    let candidate = shared_worker_capture(SHARED_DEADLINE).await;
    let control_report = CochleaReport::measure(&control.samples, CHANNELS, 48_000);
    let candidate_report = CochleaReport::measure(&candidate.samples, CHANNELS, 48_000);
    let mut failures = time_stretch_failures(
        "bound shared-worker load",
        &candidate_report,
        &control_report,
    );
    failures.extend(synchronization_failures(
        "bound shared-worker load",
        &[candidate.samples.as_slice()],
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
