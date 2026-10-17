use super::{
    super::transition::{
        wait_for_incoming_priming, wait_for_retired_decoder, wait_for_transition_settled,
    },
    *,
};
use crate::pipeline::{seek::ResumeTarget, source::StreamAudioSource};

#[kithara::test(tokio)]
async fn route_change_host_rate_delta_starts_decoder_recreate(route_pcm: RoutePcm) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    let mut gate = gate_incoming_factory(&mut fixture.source);
    fixture.source.prepare_deferred();
    gate.wait_entered().await;
    let source = &mut fixture.source;
    let origin = source.decode.active().base_offset();
    let position = source.playhead.position();
    let wake = PreparationWake::install(source);
    source.set_host_sample_rate(NonZeroU32::new(48_000).expect("host rate"));
    wait_for_source_change(source, &wake).await;
    gate.release();
    wait_for_retired_decoder(&fixture.drops, 99).await;
    let source = &mut fixture.source;
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
    assert_eq!(fixture.control.aborted_transition(), Some(transition));
    assert_eq!(fixture.control.promote_calls(), 0);
    assert_eq!(fixture.drops.lock().as_slice(), &[1, 99]);
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

    let wake = PreparationWake::install(&mut source);
    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));
    wait_for_source_change(&mut source, &wake).await;
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

    let wake = PreparationWake::install(&mut source);
    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));
    wait_for_source_change(&mut source, &wake).await;
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
async fn equal_host_rate_does_not_start_route_recreate(route_pcm: RoutePcm) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    let mut gate = gate_incoming_factory(&mut fixture.source);
    fixture.source.prepare_deferred();
    gate.wait_entered().await;
    fixture
        .source
        .set_host_sample_rate(NonZeroU32::new(consts::SAMPLE_RATE).expect("host rate"));
    let pending = fixture.source.decode.incoming_transition();
    let aborted = fixture.control.aborted_transition();
    let retired = fixture.drops.lock().clone();
    gate.release();
    wait_for_incoming_priming(&mut fixture, transition).await;

    assert_eq!(pending, Some(transition));
    assert_eq!(aborted, None);
    assert!(retired.is_empty());
    assert!(fixture.drops.lock().is_empty());
    assert!(matches!(
        fixture.source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
}

#[kithara::test(tokio)]
async fn first_matching_host_rate_latches_without_route_recreate(route_pcm: RoutePcm) {
    for building in [false, true] {
        let mut fixture = route_signal_source(&route_pcm, 0).await;
        let plan = exact_incoming_plan();
        let transition = plan.transition();
        let mut gate = if building {
            fixture.control.set_exact_plan(plan);
            fixture.control.set_exact_reader_ready();
            Some(gate_incoming_factory(&mut fixture.source))
        } else {
            None
        };
        if let Some(gate) = &mut gate {
            fixture.source.prepare_deferred();
            gate.wait_entered().await;
        }
        fixture
            .source
            .set_host_sample_rate(NonZeroU32::new(consts::SAMPLE_RATE).expect("host rate"));
        let retired = fixture.drops.lock().clone();
        drop(gate);
        if building {
            wait_for_retired_decoder(&fixture.drops, 99).await;
        }
        fixture.source.prepare_deferred();
        fixture.source.finish_deferred();

        assert!(retired.is_empty());
        assert_eq!(
            fixture.drops.lock().as_slice(),
            if building { &[99][..] } else { &[] }
        );
        assert_eq!(
            fixture.source.host_sample_rate().map(NonZeroU32::get),
            Some(consts::SAMPLE_RATE)
        );
        assert!(matches!(
            fixture.source.phase,
            crate::pipeline::source::core::OwnerPhase::Decoding
        ));
        assert!(fixture.source.decode.incoming_transition().is_none());
        assert_eq!(
            fixture.control.aborted_transition(),
            building.then_some(transition)
        );
        assert_eq!(fixture.control.promote_calls(), 0);
    }
}

#[kithara::test(tokio)]
async fn first_mismatched_host_rate_still_starts_route_recreate(route_pcm: RoutePcm) {
    let RouteFixture {
        drops, mut source, ..
    } = route_signal_source(&route_pcm, 0).await;
    let wake = PreparationWake::install(&mut source);
    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));
    wait_for_source_change(&mut source, &wake).await;
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

    enter_rebuilding(&mut source, recreate_state(1)).await;
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

    let mut decoder = TestDecoder::new(3, drops);
    decoder.defer_seek = true;
    decoder.prepare_at_byte = Some((source.shared_stream.clone(), 29));
    let seek_calls = decoder.seek_calls.clone();
    let seek_preparations = decoder.seek_preparations.clone();
    let preparations = decoder.preparations.clone();
    let settlement = decoder.settlement.clone();
    drop(source.decode.replace_active(DecoderGeneration::new(
        Box::new(decoder),
        Some(media_info(1)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    )));
    let mut events = source.emit.bus().subscribe::<AudioEvent>();
    let committed = source.playhead.position();
    let committed_byte = source.shared_stream.position();
    assert!(
        matches!(source.seek(target), Ok(crate::SeekOutcome::Pending { target: actual, .. }) if actual == target)
    );
    *settlement.lock() = Some(Ok(kithara_decode::DecoderSeekOutcome::Landed {
        landed_at: Duration::from_secs(1),
        landed_frame: 1,
        landed_byte: Some(17),
        preroll: kithara_stream::PrerollHint::NotNeeded,
    }));
    assert!(
        matches!(source.seek(target), Ok(crate::SeekOutcome::Pending { target: actual, .. }) if actual == target)
    );
    assert!(
        settlement.lock().is_none(),
        "same-target supersession drops the old completion"
    );
    source.prepare_deferred();
    assert_eq!(seek_calls.load(Ordering::Relaxed), 2);
    assert_eq!(seek_preparations.load(Ordering::Relaxed), 1);
    assert_eq!(preparations.load(Ordering::Relaxed), 0);
    assert_eq!(source.playhead.position(), committed);
    assert_eq!(source.shared_stream.position(), committed_byte);
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Seeking { .. }
    ));
    let landed = Duration::from_secs(2);
    *settlement.lock() = Some(Ok(kithara_decode::DecoderSeekOutcome::Landed {
        landed_at: landed,
        landed_frame: 2,
        landed_byte: Some(29),
        preroll: kithara_stream::PrerollHint::NotNeeded,
    }));
    source.prepare_deferred();
    assert_eq!(source.shared_stream.position(), 29);
    assert_eq!(source.playhead.position(), landed);
    assert_eq!(preparations.load(Ordering::Relaxed), 1);
    assert_eq!(seek_calls.load(Ordering::Relaxed), 2);
    assert_eq!(seek_preparations.load(Ordering::Relaxed), 2);
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    source.finish_deferred();
    let mut stages = Vec::new();
    while let Ok(envelope) = events.try_recv() {
        if let AudioEvent::SeekLifecycle { stage, .. } = envelope.event {
            stages.push(stage);
        }
    }
    assert_eq!(
        stages,
        [
            crate::SeekLifecycleStage::SeekRequest,
            crate::SeekLifecycleStage::SeekRequest,
            crate::SeekLifecycleStage::SeekApplied,
        ]
    );
}

#[kithara::test(tokio)]
async fn deferred_preparation_does_not_read_before_seek_is_applied(route_pcm: RoutePcm) {
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

    for cancelled in [false, true] {
        let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
        let mut decoder = TestDecoder::new(2, fixture.drops.clone());
        decoder.defer_seek = true;
        let seek_calls = decoder.seek_calls.clone();
        let seek_preparations = decoder.seek_preparations.clone();
        let preparations = decoder.preparations.clone();
        let settlement = decoder.settlement.clone();
        drop(fixture.source.decode.replace_active(DecoderGeneration::new(
            Box::new(decoder),
            Some(media_info(0)),
            0,
            None,
            None,
            GaplessMode::Disabled,
        )));
        let committed = fixture.source.playhead.position();
        let mut events = fixture.source.emit.bus().subscribe::<AudioEvent>();
        assert!(
            matches!(fixture.source.seek(target), Ok(crate::SeekOutcome::Pending { target: actual, .. }) if actual == target)
        );
        assert_eq!(seek_calls.load(Ordering::Relaxed), 1);
        assert_eq!(seek_preparations.load(Ordering::Relaxed), 0);
        assert_eq!(preparations.load(Ordering::Relaxed), 0);
        fixture.source.finish_deferred();
        assert!(matches!(
            events.try_recv().expect("seek request").event,
            AudioEvent::SeekLifecycle {
                stage: crate::SeekLifecycleStage::SeekRequest,
                ..
            }
        ));
        assert!(
            events.try_recv().is_err(),
            "pending cannot publish a landing"
        );
        for expected in 1..=2 {
            fixture.source.prepare_deferred();
            assert!(matches!(
                fixture.source.step_track(),
                TrackStep::Blocked(_) | TrackStep::StateChanged
            ));
            assert_eq!(seek_calls.load(Ordering::Relaxed), 1);
            assert_eq!(seek_preparations.load(Ordering::Relaxed), expected);
            assert_eq!(preparations.load(Ordering::Relaxed), 0);
            assert_eq!(fixture.source.playhead.position(), committed);
            fixture.source.finish_deferred();
            assert!(events.try_recv().is_err());
        }
        if cancelled {
            *settlement.lock() = Some(Ok(kithara_decode::DecoderSeekOutcome::Landed {
                landed_at: target,
                landed_frame: 3,
                landed_byte: Some(29),
                preroll: kithara_stream::PrerollHint::NotNeeded,
            }));
            *fixture.phase.lock() = SourcePhase::Cancelled;
        } else {
            *settlement.lock() = Some(Err(DecodeError::InvalidData {
                detail: "pending seek parse failure",
            }));
        }
        fixture.source.prepare_deferred();
        assert!(matches!(fixture.source.step_track(), TrackStep::Failed(_)));
        assert_eq!(fixture.source.playhead.position(), committed);
        assert_eq!(preparations.load(Ordering::Relaxed), 0);
        if cancelled {
            assert_eq!(
                seek_preparations.load(Ordering::Relaxed),
                2,
                "cancel preempts a ready landing"
            );
            assert!(matches!(
                fixture.source.phase,
                crate::pipeline::source::core::OwnerPhase::Failed {
                    failure: TrackFailureKind::SourceCancelled,
                    ..
                }
            ));
        } else {
            assert_eq!(seek_preparations.load(Ordering::Relaxed), 3);
            assert!(matches!(
                fixture.source.phase,
                crate::pipeline::source::core::OwnerPhase::Failed {
                    failure: TrackFailureKind::Decode {
                        kind: crate::DecodeErrorKind::InvalidData
                    },
                    error: Some(DecodeError::InvalidData {
                        detail: "pending seek parse failure"
                    }),
                }
            ));
        }
        fixture.source.finish_deferred();
        let mut rejected = 0;
        while let Ok(envelope) = events.try_recv() {
            match envelope.event {
                AudioEvent::SeekRejected { target: actual } => {
                    assert_eq!(actual, target);
                    rejected += 1;
                }
                AudioEvent::SeekLifecycle {
                    stage: crate::SeekLifecycleStage::SeekApplied,
                    ..
                }
                | AudioEvent::SeekComplete { .. } => panic!("terminal pending seek cannot land"),
                _ => {}
            }
        }
        assert_eq!(rejected, 1);
        fixture.source.prepare_deferred();
        assert!(matches!(fixture.source.step_track(), TrackStep::Failed(_)));
        fixture.source.finish_deferred();
        assert!(
            events.try_recv().is_err(),
            "terminal reentry cannot repeat rejection"
        );
        assert_eq!(
            seek_preparations.load(Ordering::Relaxed),
            if cancelled { 2 } else { 3 }
        );
    }
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
    enter_rebuilding(&mut source, recreate_state(1)).await;
    let wake = PreparationWake::install(&mut source);
    let outcome = source.seek(target).expect("owning-thread seek");
    assert!(
        matches!(outcome, crate::SeekOutcome::Pending { target: actual, .. } if actual == target)
    );
    wait_for_source_change(&mut source, &wake).await;
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
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
async fn rebuilding_decoder_variant_change_preserves_inflight_seek(route_pcm: RoutePcm) {
    let RebuildFixture {
        control,
        drops,
        mut source,
        ..
    } = test_source(1).await;
    install_test_factory(&mut source, 2, drops.clone());
    let target = Duration::from_secs(3);
    control.set_media_info(media_info(2));
    enter_rebuilding(&mut source, recreate_state(1)).await;
    let wake = PreparationWake::install(&mut source);
    let outcome = source.seek(target).expect("owning-thread seek");
    assert!(
        matches!(outcome, crate::SeekOutcome::Pending { target: actual, .. } if actual == target)
    );
    wait_for_source_change(&mut source, &wake).await;
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
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
    for for_render in [false, true] {
        assert_retired_active_rebind(&route_pcm, for_render).await;
    }
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
    let rejected = crate::pipeline::decode::transition_promotion::preparation::fixtures::install(
        &mut source.decode,
        transition,
        stale,
    );
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
async fn rebuild_factory_panic_fails_track_without_hang(route_pcm: RoutePcm) {
    let RebuildFixture { mut source, .. } = test_source(1).await;
    source.factory = DecoderFactory::new(
        |_reader, _info, _rate| panic!("decoder construction blew up"),
        None,
    );
    let wake = PreparationWake::install(&mut source);
    source.set_host_sample_rate(NonZeroU32::new(consts::ROUTE_SAMPLE_RATE).expect("host rate"));
    wait_for_source_change(&mut source, &wake).await;
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

    for panics in [false, true] {
        let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
        let plan = exact_incoming_plan();
        let transition = plan.transition();
        fixture.control.set_exact_plan(plan);
        fixture.control.set_exact_reader_ready();
        fixture.source.factory = DecoderFactory::new(
            move |_, _, _| {
                if panics {
                    panic!("incoming factory failure witness");
                }
                Err(DecodeError::InvalidData {
                    detail: "incoming factory failure witness",
                })
            },
            None,
        );
        let mut gate = gate_incoming_factory(&mut fixture.source);
        fixture.source.prepare_deferred();
        gate.wait_entered().await;
        gate.release();
        wait_for_transition_settled(&mut fixture).await;
        assert_eq!(fixture.control.aborted_transition(), Some(transition));
        assert!(fixture.source.decode.incoming_transition().is_none());
        assert!(fixture.drops.lock().is_empty());
        assert_eq!(fixture.control.promote_calls(), 0);
        assert!(matches!(
            fixture.source.step_track(),
            TrackStep::Produced(_)
        ));
        assert!(matches!(
            fixture.source.phase,
            crate::pipeline::source::core::OwnerPhase::Decoding
        ));
    }
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
        generation.stage(*chunk).expect("valid gapless PCM");
    }
    assert!(generation.has_output(), "fixture staged nothing to flush");

    generation.notify_seek();

    assert!(!generation.has_output());
}

fn assert_retained_retired_target(
    source: &StreamAudioSource<TestStream>,
    target: Duration,
    for_render: bool,
) {
    assert!(matches!(
        (source.decode.active().resume_target(), for_render),
        (Some(ResumeTarget::Position(actual)), false)
            | (Some(ResumeTarget::Render(actual)), true) if actual == target
    ));
}

async fn assert_retired_active_rebind(route_pcm: &RoutePcm, for_render: bool) {
    let RebuildFixture {
        control,
        drops,
        pools,
        mut source,
    } = test_source(0).await;
    let mut retired = TestDecoder::new(7, drops.clone());
    retired.defer_seek = true;
    retired.pending_reason = kithara_stream::PendingReason::SessionRetired;
    let stale_seek_calls = retired.seek_calls.clone();
    let stale_seek_preparations = retired.seek_preparations.clone();
    let stale_preparations = retired.preparations.clone();
    drop(source.decode.replace_active(DecoderGeneration::new(
        Box::new(retired),
        Some(media_info(0)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    )));
    assert_eq!(drops.lock().as_slice(), &[1]);
    assert_eq!(
        source.decode.active().media_info(),
        control.media_info.lock().as_ref()
    );
    *control.format_range.lock() = Some(64..96);
    let builds = Arc::new(AtomicU64::new(0));
    let factory_builds = builds.clone();
    let factory_drops = drops.clone();
    let factory_pools = pools.clone();
    let factory_pcm = route_pcm.clone();
    source.factory = DecoderFactory::new(
        move |reader, info, _rate| {
            factory_builds.fetch_add(1, Ordering::Relaxed);
            assert_eq!(
                info,
                Some(media_info(0)),
                "metadata equality cannot suppress rebind"
            );
            assert_eq!(
                reader.byte_len(),
                Some(4096 - 64),
                "replacement uses the current header"
            );
            let mut decoder = RouteSignalDecoder::new(
                &factory_pcm,
                8,
                consts::SAMPLE_RATE,
                None,
                None,
                factory_drops.clone(),
                factory_pools.clone(),
            );
            decoder.seek_landing = Some(800);
            Ok(Box::new(decoder))
        },
        None,
    );
    let target = Duration::from_millis(20);
    let wake = PreparationWake::install(&mut source);
    let outcome = if for_render {
        source.seek_for_render(target)
    } else {
        source.seek(target)
    };
    assert!(matches!(outcome, Ok(crate::SeekOutcome::Pending {
        target: actual, reason: kithara_stream::PendingReason::SessionRetired,
    }) if actual == target));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Seeking { .. }
    ));
    assert_retained_retired_target(&source, target, for_render);
    assert_eq!(stale_seek_calls.load(Ordering::Relaxed), 1);
    assert_eq!(stale_seek_preparations.load(Ordering::Relaxed), 0);
    assert_eq!(stale_preparations.load(Ordering::Relaxed), 0);
    assert!(matches!(source.step_track(), TrackStep::Blocked(_)));
    wait_for_source_change(&mut source, &wake).await;
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(source.decode.active().base_offset(), 64);
    assert_eq!(
        source.decode.active().media_info(),
        control.media_info.lock().as_ref()
    );
    assert_retained_retired_target(&source, target, for_render);
    assert_eq!(builds.load(Ordering::Relaxed), 1);
    assert_eq!(drops.lock().as_slice(), &[1, 7]);
    assert_eq!(stale_seek_preparations.load(Ordering::Relaxed), 0);
    assert_eq!(stale_preparations.load(Ordering::Relaxed), 0);
    source.prepare_deferred();
    let TrackStep::Produced(fetch) = source.step_track() else {
        panic!("replacement must produce its real trimmed PCM");
    };
    let chunk = produced_data(fetch);
    let expected_frame = if for_render { 881 } else { 882 };
    let expected_end = 800 + consts::ROUTE_CHUNK_FRAMES;
    assert_eq!(
        chunk.meta.frame_offset,
        u64::try_from(expected_frame).expect("target frame")
    );
    assert_eq!(chunk.frames(), expected_end - expected_frame);
    assert_eq!(
        chunk.meta.timestamp,
        spec(consts::SAMPLE_RATE)
            .duration_for(u64::try_from(expected_frame).expect("target frame"))
            .expect("trimmed source position")
    );
    assert_eq!(
        &*chunk.samples,
        &route_pcm[0][expected_frame * 2..expected_end * 2]
    );
    assert!(
        source.decode.active().resume_target().is_none(),
        "trim intent is consumed once"
    );
    source.prepare_deferred();
    source.finish_deferred();
    assert_eq!(builds.load(Ordering::Relaxed), 1);
    assert_eq!(stale_seek_calls.load(Ordering::Relaxed), 1);
    assert_eq!(stale_seek_preparations.load(Ordering::Relaxed), 0);
    assert_eq!(stale_preparations.load(Ordering::Relaxed), 0);
    assert_eq!(drops.lock().as_slice(), &[1, 7]);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn incoming_retired_seek_fails_only_its_slot_before_publication(route_pcm: RoutePcm) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    let revision = fixture.source.decode.discontinuity().revision();
    let builds = Arc::new(AtomicU64::new(0));
    let factory_builds = builds.clone();
    fixture.source.factory = DecoderFactory::new(
        move |_reader, _info, _rate| {
            factory_builds.fetch_add(1, Ordering::Relaxed);
            Err(DecodeError::InvalidData {
                detail: "retired incoming cannot reopen active",
            })
        },
        None,
    );
    let mut retired = TestDecoder::new(77, fixture.drops.clone());
    retired.defer_seek = true;
    retired.pending_reason = kithara_stream::PendingReason::SessionRetired;
    let seek_calls = retired.seek_calls.clone();
    let seek_preparations = retired.seek_preparations.clone();
    let preparations = retired.preparations.clone();
    let mut incoming = DecoderGeneration::new(
        Box::new(retired),
        Some(media_info(1)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    );
    assert!(matches!(
        incoming.seek(Duration::ZERO, GaplessMode::Disabled),
        Ok(kithara_decode::DecoderSeekOutcome::Pending(
            kithara_stream::PendingReason::SessionRetired
        ))
    ));
    incoming.trim_to(ResumeTarget::Position(Duration::ZERO));
    assert!(
        fixture
            .source
            .decode
            .begin_incoming(
                transition,
                OutgoingFrontier::Exact {
                    frame: 0,
                    rate: consts::SAMPLE_RATE,
                }
            )
            .is_none()
    );
    assert!(
        crate::pipeline::decode::transition_promotion::preparation::fixtures::install(
            &mut fixture.source.decode,
            transition,
            incoming
        )
        .is_none()
    );
    let mut events = fixture.source.emit.bus().subscribe::<AudioEvent>();
    fixture.source.prepare_deferred();
    let (completed_transition, prepared) = loop {
        hang_tick!();
        if let Some(completed) = fixture.source.decode.poll_incoming_build() {
            break completed;
        }
        kithara_platform::tokio::task::yield_now().await;
    };
    assert_eq!(completed_transition, transition);
    let prepared = prepared.expect("retired incoming preparation must return its owned generation");
    assert!(matches!(
        &prepared.outcome,
        Ok(crate::pipeline::decode::transition::IncomingPrime::Failed)
    ));
    assert_eq!(seek_calls.load(Ordering::Relaxed), 1);
    assert_eq!(seek_preparations.load(Ordering::Relaxed), 1);
    assert_eq!(preparations.load(Ordering::Relaxed), 0);
    assert!(!fixture.source.decode.incoming_is_priming(transition));
    assert_eq!(
        fixture.source.decode.incoming_transition(),
        Some(transition)
    );
    assert!(fixture.source.decode.incoming_staged_span().is_none());
    assert_eq!(fixture.control.promote_calls(), 0);
    assert_eq!(fixture.control.aborted_transition(), None);
    assert!(fixture.drops.lock().is_empty());
    assert!(
        fixture
            .source
            .decode
            .install_incoming(transition, prepared.generation)
            .is_none()
    );
    fixture
        .source
        .decode
        .finish_incoming_preparation(prepared.outcome);
    fixture.source.prepare_deferred();
    fixture.source.finish_deferred();
    assert_eq!(fixture.control.aborted_transition(), Some(transition));
    assert!(fixture.source.decode.incoming_transition().is_none());
    assert_eq!(fixture.drops.lock().as_slice(), &[77]);
    assert_eq!(
        fixture.source.decode.active().media_info(),
        Some(&media_info(0))
    );
    assert_eq!(fixture.source.decode.discontinuity().revision(), revision);
    assert!(matches!(
        fixture.source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(builds.load(Ordering::Relaxed), 0);
    let TrackStep::Produced(fetch) = fixture.source.step_track() else {
        panic!("active PCM survives incoming retirement");
    };
    let chunk = produced_data(fetch);
    assert_eq!(chunk.meta.frame_offset, 0);
    assert_eq!(chunk.meta.source_revision, revision);
    assert_eq!(
        &*chunk.samples,
        &route_pcm[0][..consts::ROUTE_CHUNK_FRAMES * 2]
    );
    fixture.source.prepare_deferred();
    fixture.source.finish_deferred();
    assert_eq!(builds.load(Ordering::Relaxed), 0);
    assert_eq!(seek_preparations.load(Ordering::Relaxed), 1);
    assert_eq!(preparations.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.control.promote_calls(), 0);
    assert_eq!(fixture.drops.lock().as_slice(), &[77]);
    while let Ok(envelope) = events.try_recv() {
        assert!(!matches!(
            envelope.event,
            AudioEvent::SeekLifecycle { .. }
                | AudioEvent::SeekComplete { .. }
                | AudioEvent::TrackFailed { .. }
        ));
    }
}
