#![cfg(not(target_arch = "wasm32"))]

use kithara_integration_tests::mock::gapless::*;

#[kithara::test(native, tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
async fn single_track_media_only_strips_leading_priming(
    temp_dir: TestTempDir,
    #[future(awt)] primed: (TestServerHelper, CreatedHls),
) {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .gapless(true)
            .gapless_mode(GaplessMode::MediaOnly)
            .build(),
        GAPLESS_SAMPLE_RATE,
    )
    .await;

    let resource = create_resource(&harness, &primed.1, temp_dir.path()).await;

    let [single] = load_tagged_queue(&harness, [resource]).await;

    let (rendered, events) = render_until_item_end(&harness, single).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let _ = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item } if item.id() == single
        )
    })
    .expect("ItemDidPlayToEnd must fire for the single-track fixture");

    let audio_end = audio_end_frame(&left, 0.05);
    let visible = expected_visible_frames(AAC_GAPLESS_ENCODER_DELAY, 0);
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");
    assert_close_to(
        audio_end,
        visible,
        sample_rate / 10,
        "rendered length must approximate visible (no leading priming)",
        &events,
    );

    let head_rms = rms(&left, 0, AAC_FRAME_SAMPLES.min(left.len()));
    assert!(
        head_rms > 0.2,
        "head RMS too low — leading priming likely not trimmed: head_rms={head_rms:.4}, \
         events={events:?}"
    );
    harness.close().await;
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_play=debug,kithara_stream=debug")
)]
async fn two_tracks_gapless_no_click_with_media_only_zero_crossfade(
    temp_dir: TestTempDir,
    #[future(awt)] trimmed: (TestServerHelper, CreatedHls),
    #[future(awt)] trimmed_next: (TestServerHelper, CreatedHls),
) {
    let visible = expected_visible_frames(AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_TRAILING_DELAY);
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .gapless(true)
            .gapless_mode(GaplessMode::MediaOnly)
            .build(),
        GAPLESS_SAMPLE_RATE,
    )
    .await;

    let first = create_resource(&harness, &trimmed.1, temp_dir.path()).await;
    let second = create_resource(&harness, &trimmed_next.1, temp_dir.path()).await;

    let [first_id, second_id] = load_tagged_queue(&harness, [first, second]).await;

    let (rendered, events) = render_until_item_end(&harness, second_id).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");

    let item1_end = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item } if item.id() == first_id
        )
    })
    .expect("first item must emit ItemDidPlayToEnd");
    let _ = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item } if item.id() == second_id
        )
    })
    .expect("second item must emit ItemDidPlayToEnd");

    let joined_expected = visible.saturating_mul(2);
    let audio_end = audio_end_frame(&left, 0.05);
    assert_close_to(
        audio_end,
        joined_expected,
        sample_rate / 4,
        "joined rendered length must approximate 2 × visible frames",
        &events,
    );

    let search_start = item1_end.saturating_sub(BLOCK_FRAMES * 2);
    let search_end = item1_end.saturating_add(BLOCK_FRAMES * 2).min(left.len());
    let max_silence = max_silence_run(&left, search_start, search_end, SILENCE_THRESHOLD);
    assert!(
        max_silence < BLOCK_FRAMES,
        "boundary silence run must stay below one render block ({BLOCK_FRAMES} frames), \
         got {max_silence} frames in [{search_start}..{search_end}); events={events:?}",
    );

    if item1_end > 0 && item1_end < left.len() {
        let step = (left[item1_end] - left[item1_end - 1]).abs();
        assert!(
            step < 0.5,
            "boundary discontinuity {step:.3} too large — likely an audible click; \
             item1_end={item1_end}, events={events:?}"
        );
    }
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
async fn two_tracks_gapless_stitch_continuity_metric(
    temp_dir: TestTempDir,
    #[future(awt)] trimmed: (TestServerHelper, CreatedHls),
    #[future(awt)] trimmed_stitch: (TestServerHelper, CreatedHls),
) {
    let stitch_frame = crate::gapless_common::generated_aac_elst_visible_frames();
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .gapless(true)
            .gapless_mode(GaplessMode::MediaOnly)
            .build(),
        GAPLESS_SAMPLE_RATE,
    )
    .await;

    let first = create_resource(&harness, &trimmed.1, temp_dir.path()).await;
    let second = create_resource(&harness, &trimmed_stitch.1, temp_dir.path()).await;

    let [_, second_id] = load_tagged_queue(&harness, [first, second]).await;

    let (rendered, events) = render_until_item_end(&harness, second_id).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));

    assert!(
        left.len() > stitch_frame + ContinuityMetric::HALF_WINDOW_FRAMES,
        "rendered PCM must cover the stitch window; left_frames={}, stitch_frame={stitch_frame}, events={events:?}",
        left.len()
    );

    let switch_peak = peak_first_diff(&left, stitch_frame, ContinuityMetric::HALF_WINDOW_FRAMES);
    let (control_peak, control_count) = gapless_control_peak(&left, stitch_frame);
    assert!(
        control_count >= ContinuityMetric::MIN_CONTROL_WINDOWS,
        "gapless continuity metric needs at least {} same-track control windows, got {control_count}",
        ContinuityMetric::MIN_CONTROL_WINDOWS,
    );
    let ratio = switch_peak / control_peak.max(f32::EPSILON);
    println!(
        "GAPLESS_CONTINUITY switch_peak={switch_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
    );
    assert!(
        ratio < ContinuityMetric::MAX_RATIO,
        "gapless stitch discontinuity {switch_peak:.6} is {ratio:.1}x the worst same-track control window {control_peak:.6}",
    );
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
async fn fused_gapless_tail_compensation_restores_exact_length_at_stitch(
    fused_seam_stereo: Vec<f32>,
) {
    let compensated = render_synthetic_fused_deficit_seam(true, &fused_seam_stereo).await;
    let uncompensated = render_synthetic_fused_deficit_seam(false, &fused_seam_stereo).await;

    let stitch_frame = FUSED_FIXTURE_IDEAL_DEVICE_FRAMES;
    let compensated_db = seam_step_db(&compensated.left, stitch_frame);
    let uncompensated_db = seam_step_db(&uncompensated.left, stitch_frame - 1);
    println!(
        "FUSED_GAPLESS_DEFICIT compensated_db={compensated_db:.2} uncompensated_db={uncompensated_db:.2}"
    );

    assert_eq!(compensated.first_frames, FUSED_FIXTURE_IDEAL_DEVICE_FRAMES);
    assert_eq!(
        uncompensated.first_frames,
        FUSED_FIXTURE_IDEAL_DEVICE_FRAMES - 1
    );
    assert!(
        compensated_db < -30.0,
        "compensated seam residual too high: {compensated_db:.2} dB; events={:?}",
        compensated.events
    );
    assert!(
        uncompensated_db > -32.0,
        "uncompensated control seam should expose the one-frame deficit: {uncompensated_db:.2} dB; events={:?}",
        uncompensated.events
    );
    assert!(
        compensated_db + 1.0 < uncompensated_db,
        "tail-side compensation must measurably improve the seam: compensated={compensated_db:.2}, uncompensated={uncompensated_db:.2}"
    );
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[kithara::test(native, tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
async fn apple_fused_gapless_fixture_keeps_device_rate_seam_metric(
    temp_dir: TestTempDir,
    #[future(awt)] apple_probe: (TestServerHelper, CreatedHls),
    #[future(awt)] apple_first: (TestServerHelper, CreatedHls),
    #[future(awt)] apple_next: (TestServerHelper, CreatedHls),
) {
    let source_stitch_frame = APPLE_FUSED_DEFICIT_SOURCE_FRAMES;
    let floor_frames = usize::try_from(floor_scaled_frames(
        source_stitch_frame,
        FUSED_FIXTURE_DEVICE_RATE,
        FUSED_FIXTURE_SOURCE_RATE,
    ))
    .expect("fixture length fits usize");
    let ceil_frames = usize::try_from(ceil_scaled_frames(
        source_stitch_frame,
        FUSED_FIXTURE_DEVICE_RATE,
        FUSED_FIXTURE_SOURCE_RATE,
    ))
    .expect("fixture length fits usize");
    assert_eq!(
        (floor_frames, ceil_frames),
        (284_212, 284_213),
        "fixture must keep the selected one-frame-deficit search geometry"
    );

    let probe_worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let probe = create_apple_fused_resource(&probe_worker, &apple_probe.1, temp_dir.path()).await;
    let probe = kithara_integration_tests::mock::open_resource(&probe)
        .await
        .expect("open Apple fused probe");
    let probe = drain_resource_to_eof(probe).await;
    assert!(
        (floor_frames..=ceil_frames).contains(&probe.output_frames),
        "visible length must be the scaled ratio rounded either way: got {}, expected {floor_frames} or {ceil_frames}",
        probe.output_frames
    );
    drop(probe_worker);

    let pending_decision = render_apple_fused_deficit_seam(
        &apple_first.1,
        &apple_next.1,
        temp_dir.path(),
        probe.output_frames,
    )
    .await;

    println!(
        "APPLE_FUSED_TAIL_SEAM measured_frames={} rounds_to={floor_frames}..={ceil_frames} \
         seam_db={:.2} control_db={:.2} head_db={:.2} stitch_frame={} nearby={:?}",
        probe.output_frames,
        pending_decision.seam_db,
        pending_decision.control_db,
        pending_decision.head_db,
        pending_decision.stitch_frame,
        pending_decision.nearby_db
    );

    assert!(pending_decision.seam_db.is_finite());
    let bound = pending_decision.control_db.max(pending_decision.head_db);
    assert!(
        pending_decision.seam_db < bound,
        "fused seam must not exceed the material's own steps: seam={:.2} dB, control={:.2} dB, head={:.2} dB",
        pending_decision.seam_db,
        pending_decision.control_db,
        pending_decision.head_db
    );
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
async fn render_apple_fused_deficit_seam(
    first_source: &CreatedHls,
    second_source: &CreatedHls,
    cache_dir: &std::path::Path,
    stitch_frame: usize,
) -> AppleFusedSeamRender {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .gapless(true)
            .gapless_mode(GaplessMode::MediaOnly)
            .build(),
        FUSED_FIXTURE_DEVICE_RATE,
    )
    .await;

    let first = create_apple_fused_resource(harness.worker(), first_source, cache_dir).await;
    let second = create_apple_fused_resource(harness.worker(), second_source, cache_dir).await;

    let [first_id, _second_id] = load_tagged_queue(&harness, [first, second]).await;

    let control_post_roll_blocks =
        (ContinuityMetric::CONTROL_STRIDE_FRAMES / BLOCK_FRAMES) + POST_ROLL_BLOCKS;
    let (rendered, events) =
        render_until_item_end_with_post_roll(&harness, first_id, control_post_roll_blocks).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let _ = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item } if item.id() == first_id
        )
    })
    .expect("first fused item must emit ItemDidPlayToEnd");

    assert!(
        stitch_frame < left.len(),
        "fused stitch frame must be inside rendered PCM; stitch_frame={stitch_frame}, left_frames={}, events={events:?}",
        left.len()
    );

    let seam_db = seam_step_db(&left, stitch_frame);
    let (control_peak, control_count) = gapless_control_peak(&left, stitch_frame);
    let control_db = amplitude_db(control_peak);
    assert!(
        control_count >= ContinuityMetric::MIN_CONTROL_WINDOWS,
        "Apple fused seam metric needs at least {} same-track control windows, got {control_count}",
        ContinuityMetric::MIN_CONTROL_WINDOWS
    );
    let result = AppleFusedSeamRender {
        control_db,
        head_db: seam_step_db(&left, stitch_frame.saturating_add(1)),
        nearby_db: nearby_seam_step_db(&left, stitch_frame),
        seam_db,
        stitch_frame,
    };
    harness.close().await;
    result
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
async fn disabled_gapless_mode_keeps_full_decoded_length(
    temp_dir: TestTempDir,
    #[future(awt)] trimmed: (TestServerHelper, CreatedHls),
) {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .gapless(true)
            .gapless_mode(GaplessMode::Disabled)
            .build(),
        GAPLESS_SAMPLE_RATE,
    )
    .await;

    let resource = create_resource(&harness, &trimmed.1, temp_dir.path()).await;

    let [only] = load_tagged_queue(&harness, [resource]).await;

    let (rendered, events) = render_until_item_end(&harness, only).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let _ = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item } if item.id() == only
        )
    })
    .expect("ItemDidPlayToEnd must fire");

    let audio_end = audio_end_frame(&left, 0.05);
    let total_decoded = expected_total_decoded_frames();
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");
    assert_close_to(
        audio_end,
        total_decoded,
        sample_rate / 10,
        "Disabled mode must pass through full decoded PCM (no leading/trailing trim)",
        &events,
    );
    harness.close().await;
}
