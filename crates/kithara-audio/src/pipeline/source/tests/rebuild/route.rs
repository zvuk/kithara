use super::*;

#[kithara::test(tokio)]
async fn route_change_host_rate_delta_starts_decoder_recreate(route_pcm: RoutePcm) {
    let RouteFixture { mut source, .. } =
        route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let origin = source.decode.active().base_offset();
    let position = source.playhead.position();
    source.set_host_sample_rate(NonZeroU32::new(48_000).expect("host rate"));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(source.host_sample_rate().map(NonZeroU32::get), Some(48_000));
    assert_eq!(source.playhead.position(), position);
    assert_eq!(
        source.decode.active().decoder().spec().sample_rate.get(),
        48_000
    );
    assert!(source.decode.incoming_transition().is_none());
    assert_eq!(source.decode.active().base_offset(), origin);
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
}

#[kithara::test(tokio)]
async fn route_change_resumes_from_the_rendered_source_frontier(route_pcm: RoutePcm) {
    let RouteFixture { mut source, .. } =
        route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let mut route_recreated = false;
    let chunk = next_decoded_chunk(&mut source, &mut route_recreated);

    let rendered_frame =
        u64::try_from(consts::ROUTE_CHUNK_FRAMES / 2).expect("rendered fixture frame fits u64");
    let rendered = spec(consts::SAMPLE_RATE)
        .duration_for(rendered_frame)
        .expect("rendered fixture position fits Duration");

    assert_ne!(
        source.resume.decode_head(),
        Some((rendered_frame, consts::SAMPLE_RATE)),
        "the fixture must distinguish raw decode progress from rendered progress"
    );
    assert_eq!(
        chunk.meta.end_timestamp,
        spec(consts::SAMPLE_RATE)
            .duration_for(
                u64::try_from(consts::ROUTE_CHUNK_FRAMES).expect("route chunk frames fit u64"),
            )
            .expect("route chunk duration fits Duration")
    );
    source.commit_source_end(
        SourceEnd::new(
            rendered_frame,
            NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate is non-zero"),
        ),
        chunk.meta,
    );

    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    let chunk = next_decoded_chunk(&mut source, &mut route_recreated);
    assert_eq!(
        chunk.meta.timestamp, rendered,
        "route recreation resumes at rendered source progress"
    );
    let span = chunk
        .meta
        .source_span
        .expect("rebuilt chunk source mapping");
    assert_eq!(span.start(), rendered_frame);
    assert_eq!(span.sample_rate().get(), consts::SAMPLE_RATE);
    assert_eq!(span.output_frames(), u64::from(chunk.meta.frames));
    let next = next_decoded_chunk(&mut source, &mut route_recreated);
    let next_span = next.meta.source_span.expect("next chunk source mapping");
    assert_eq!(next.meta.timestamp, chunk.meta.end_timestamp);
    assert_eq!(
        next_span.source_ratio_at(0),
        span.source_ratio_at(span.output_frames())
    );
}

#[kithara::test(tokio)]
async fn route_change_recreate_preserves_position_and_output_rate_continuity_metric(
    route_pcm: RoutePcm,
) {
    let RouteFixture { mut source, .. } =
        route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let mut left = Vec::new();
    let mut route_recreated = false;

    for _ in 0..8 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        assert_eq!(chunk.meta.spec.sample_rate.get(), consts::SAMPLE_RATE);
        append_left_channel(&mut left, &chunk);
        source
            .playhead
            .advance(&crate::audio::chunk_position(&chunk.meta));
    }

    let route_frame = left.len();
    let route_position = source.playhead.position();
    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));

    let mut first_route_timestamp = None;
    let mut saw_new_rate = false;
    for _ in 0..8 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        if first_route_timestamp.is_none() {
            first_route_timestamp = Some(chunk.meta.timestamp);
        }
        saw_new_rate |= chunk.meta.spec.sample_rate.get() == consts::ROUTE_SAMPLE_RATE;
        append_left_channel(&mut left, &chunk);
        source
            .playhead
            .advance(&crate::audio::chunk_position(&chunk.meta));
    }

    assert!(
        route_recreated,
        "route change must enter recreate machinery"
    );
    assert!(
        saw_new_rate,
        "route-change output chunks must report the new host rate"
    );
    assert_eq!(
        source.decode.active().decoder().spec().sample_rate.get(),
        consts::ROUTE_SAMPLE_RATE
    );
    let first_route_timestamp =
        first_route_timestamp.expect("route change should produce post-route PCM");
    let drift_ns = first_route_timestamp.abs_diff(route_position).as_nanos();
    assert!(
        drift_ns <= 1_000_000,
        "route recreate drifted by {drift_ns} ns from {route_position:?} to {first_route_timestamp:?}",
    );

    let route_peak = peak_first_diff(&left, route_frame, 64);
    let control_peak = peak_first_diff(&left, consts::ROUTE_CHUNK_FRAMES * 4, 64);
    let ratio = route_peak / control_peak.max(f32::EPSILON);
    println!(
        "S_ROUTE_CONTINUITY route_peak={route_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
    );
    assert!(
        ratio < 2.0,
        "route-change discontinuity {route_peak:.6} is {ratio:.1}x the control boundary {control_peak:.6}",
    );
}

/// A route change swaps the resampler over the SAME container, so the
/// rebuilt demuxer has to be rooted where the live one is — the container
/// origin the running session was installed at. Deriving that origin from
/// the seek anchor instead hands an init-bearing demuxer a media byte; the
/// recreate then fails outright and takes the track with it.
#[kithara::test(tokio)]
async fn route_change_recreate_roots_the_demuxer_at_the_container_origin(route_pcm: RoutePcm) {
    let RouteFixture { mut source, .. } = route_source(
        &route_pcm,
        RouteParams {
            chunks_before_eof: None,
            gapless: None,
            incoming_chunks_before_eof: None,
            active_timeline_gap: 0,
            incoming_timeline_gap: 0,
            initial_host_rate: consts::SAMPLE_RATE,
            segmented: true,
        },
    )
    .await;

    let mut route_recreated = false;
    for _ in 0..4 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        source
            .playhead
            .advance(&crate::audio::chunk_position(&chunk.meta));
    }
    let resume_anchor = source
        .shared_stream
        .seek_time_anchor(source.playhead.position())
        .ok()
        .flatten()
        .expect("segmented source resolves an anchor for the resume position");
    assert_ne!(
        resume_anchor.byte_offset,
        source.decode.active().base_offset(),
        "fixture precondition: the resume anchor must be a media byte, not the container origin"
    );

    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(
        source.host_sample_rate().map(NonZeroU32::get),
        Some(consts::ROUTE_SAMPLE_RATE)
    );
    assert_eq!(
        source.decode.active().base_offset(),
        0,
        "route change reuses container origin"
    );

    let mut saw_new_rate = false;
    for _ in 0..4 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        saw_new_rate |= chunk.meta.spec.sample_rate.get() == consts::ROUTE_SAMPLE_RATE;
        source
            .playhead
            .advance(&crate::audio::chunk_position(&chunk.meta));
    }
    assert!(
        saw_new_rate,
        "the rebuilt decoder must deliver the new host rate"
    );
}

#[kithara::test(tokio)]
async fn equal_host_rate_does_not_start_route_recreate() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(0).await;
    source.set_host_sample_rate(NonZeroU32::new(consts::SAMPLE_RATE).expect("host rate"));
    assert!(drops.lock().is_empty());
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
}

#[kithara::test(tokio)]
async fn first_matching_host_rate_latches_without_route_recreate(route_pcm: RoutePcm) {
    let RouteFixture {
        drops, mut source, ..
    } = route_signal_source(&route_pcm, 0).await;
    source.set_host_sample_rate(NonZeroU32::new(consts::SAMPLE_RATE).expect("host rate"));
    assert!(drops.lock().is_empty());
    assert_eq!(
        source.host_sample_rate().map(NonZeroU32::get),
        Some(consts::SAMPLE_RATE)
    );
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
}

#[kithara::test(tokio)]
async fn first_mismatched_host_rate_still_starts_route_recreate(route_pcm: RoutePcm) {
    let RouteFixture {
        drops, mut source, ..
    } = route_signal_source(&route_pcm, 0).await;
    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));
    assert_eq!(drops.lock().as_slice(), &[1]);
    assert_eq!(
        source.decode.output_spec().sample_rate.get(),
        consts::ROUTE_SAMPLE_RATE
    );
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_seek_epoch_supersedes_completion() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    install_test_factory(&mut source, 2, drops.clone());
    let target = Duration::from_secs(3);

    enter_rebuilding(&mut source, recreate_state(1));
    let outcome = source.seek(target).expect("owning-thread seek");
    assert!(matches!(outcome, crate::SeekOutcome::Landed { .. }));
    assert!(
        matches!(outcome, crate::SeekOutcome::Landed { target: actual, .. } if actual == target)
    );
    assert_eq!(source.playhead.position(), target);
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(1)
    );
    assert_eq!(drops.lock().as_slice(), &[1]);
    source.finish_deferred();
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
}

#[kithara::test(tokio)]
async fn deferred_preparation_does_not_read_before_seek_is_applied() {
    let RebuildFixture {
        mut source, drops, ..
    } = test_source(0).await;
    let decoder = TestDecoder::new(2, drops);
    let preparations = decoder.preparations.clone();
    drop(source.decode.replace_active(DecoderGeneration::new(
        Box::new(decoder),
        Some(media_info(0)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    )));
    source.prepare_deferred();
    assert_eq!(preparations.swap(0, Ordering::Relaxed), 1);
    let target = Duration::from_secs(3);
    let outcome = source.seek(target).expect("synchronous seek");
    assert_eq!(preparations.load(Ordering::Relaxed), 0);
    assert!(
        matches!(outcome, crate::SeekOutcome::Landed { target: position, .. } if position == target)
    );
    assert_eq!(source.playhead.position(), target);
    assert_eq!(preparations.load(Ordering::Relaxed), 0);
}

#[kithara::test(tokio)]
async fn completed_seek_is_consumed_when_landing_bytes_are_no_longer_ready(route_pcm: RoutePcm) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let target = Duration::from_millis(10);
    let outcome = fixture.source.seek(target).expect("synchronous landing");
    assert!(matches!(outcome, crate::SeekOutcome::Landed { .. }));
    *fixture.phase.lock() = SourcePhase::Waiting;
    assert_eq!(fixture.source.playhead.position(), target);
    assert!(matches!(
        fixture.source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_variant_change_supersedes_completion() {
    let RebuildFixture {
        control,
        drops,
        mut source,
        ..
    } = test_source(1).await;
    install_test_factory(&mut source, 2, drops.clone());
    let target = Duration::from_secs(3);
    control.set_media_info(media_info(2));
    enter_rebuilding(&mut source, recreate_state(1));
    let outcome = source.seek(target).expect("owning-thread seek");
    assert!(matches!(outcome, crate::SeekOutcome::Landed { .. }));
    assert!(
        matches!(outcome, crate::SeekOutcome::Landed { target: actual, .. } if actual == target)
    );
    assert_eq!(source.playhead.position(), target);
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(2)
    );
    assert_eq!(drops.lock().as_slice(), &[1, 2]);
    source.finish_deferred();
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_variant_change_preserves_inflight_seek() {
    let RebuildFixture {
        control,
        drops,
        mut source,
        ..
    } = test_source(1).await;
    install_test_factory(&mut source, 2, drops.clone());
    let target = Duration::from_secs(3);
    control.set_media_info(media_info(2));
    enter_rebuilding(&mut source, recreate_state(1));
    let outcome = source.seek(target).expect("owning-thread seek");
    assert!(matches!(outcome, crate::SeekOutcome::Landed { .. }));
    assert!(
        matches!(outcome, crate::SeekOutcome::Landed { target: actual, .. } if actual == target)
    );
    assert_eq!(source.playhead.position(), target);
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(2)
    );
    assert_eq!(drops.lock().as_slice(), &[1, 2]);
    source.finish_deferred();
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
}

#[kithara::test(tokio)]
async fn stale_rebuild_completion_retires_decoder_shell_side() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    install_test_factory(&mut source, 2, drops.clone());

    let transition = exact_incoming_plan().transition();
    let stale = DecoderGeneration::new(
        Box::new(TestDecoder::new(3, drops.clone())),
        Some(media_info(1)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    );
    assert!(drops.lock().is_empty());
    let rejected = source.decode.install_incoming(transition, stale);
    assert!(rejected.is_some());
    drop(rejected);
    assert_eq!(drops.lock().as_slice(), &[3]);
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(source.decode.incoming_transition(), None);
}

/// A decoder factory that panics during construction must not strand the
/// FSM in `RebuildingDecoder` forever. The rebuild port catches the panic,
/// pushes a `SoftFailed` completion, and wakes the worker.
#[kithara::test(tokio)]
async fn rebuild_factory_panic_fails_track_without_hang() {
    let RebuildFixture { mut source, .. } = test_source(1).await;
    source.factory = DecoderFactory::new(
        |_reader, _info, _rate| panic!("decoder construction blew up"),
        None,
    );
    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Failed {
            failure: TrackFailureKind::RecreateFailed { offset: 0 },
            error: Some(DecodeError::InvalidData {
                detail: "decoder factory panicked"
            })
        }
    ));
    assert!(matches!(
        source.step_track(),
        TrackStep::Failed(TrackFailureKind::RecreateFailed { offset: 0 })
    ));
    source.finish_deferred();
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Failed {
            failure: TrackFailureKind::RecreateFailed { offset: 0 },
            error: None
        }
    ));
}

#[kithara::test]
fn a_seek_releases_its_buffered_chunks_off_rt(route_pcm: RoutePcm) {
    const STAGED: usize = 3;
    let pools = pools();

    let mut generation = DecoderGeneration::new(
        Box::new(RouteSignalDecoder::new(
            &route_pcm,
            1,
            48_000,
            None,
            None,
            Arc::default(),
            pools,
        )),
        None,
        0,
        None,
        None,
        GaplessMode::Disabled,
    );
    for _ in 0..STAGED {
        let DecoderChunkOutcome::Chunk(chunk) = generation.next_chunk().expect("fixture chunk")
        else {
            panic!("the route-signal fixture produces chunks");
        };
        generation.stage(*chunk);
    }
    assert!(generation.has_output(), "fixture staged nothing to flush");

    generation.notify_seek();

    assert!(!generation.has_output());
}
