use super::*;
use crate::pipeline::stream::shared::SharedStream;

#[kithara::test(tokio)]
async fn retired_generations_are_all_reclaimed_after_a_burst() {
    let RebuildFixture { mut source, .. } = test_source(0).await;
    let drops = Arc::new(Mutex::new(Vec::new()));
    let initial = DecoderGeneration::new(
        Box::new(TestDecoder::new(0, Arc::clone(&drops))),
        None,
        0,
        None,
        None,
        GaplessMode::Disabled,
    );
    drop(source.decode.replace_active(initial));
    assert!(
        drops.lock().is_empty(),
        "the active generation is still owned"
    );
    for id in 1..=5 {
        let generation = DecoderGeneration::new(
            Box::new(TestDecoder::new(id, Arc::clone(&drops))),
            None,
            0,
            None,
            None,
            GaplessMode::Disabled,
        );
        drop(source.decode.replace_active(generation));
    }
    drop(source);
    let mut dropped = drops.lock().clone();
    dropped.sort_unstable();
    assert_eq!(dropped, (0..=5).collect::<Vec<_>>());
}

#[kithara::test(tokio)]
async fn checked_seek_defers_more_than_64_pcm_chunks_without_leaking() {
    let config = PoolConfig::builder()
        .max_buffers(128)
        .max_retained_capacity(1)
        .build();
    let pools = pools_with(1024 * 1024, config, config);
    let baseline = pools.stats().allocated_bytes;
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(0).await;
    let mut generation = DecoderGeneration::new(
        Box::new(TestDecoder::new(7, drops)),
        None,
        0,
        None,
        None,
        GaplessMode::Disabled,
    );
    for _ in 0..65 {
        generation.stage(AudioChunk::new(
            AudioChunkInfo::default(),
            sample_buffer(&pools, &[0.0, 0.0]),
        ));
    }
    assert!(generation.has_output());
    let old = source.decode.replace_active(generation);
    drop(old);
    source.finish_deferred();
    assert!(pools.stats().allocated_bytes > baseline);

    assert!(
        pools.stats().allocated_bytes > baseline,
        "owner retains all 65 chunks before seek"
    );
    source
        .seek(Duration::from_secs(1))
        .expect("synchronous owner seek");
    source.finish_deferred();
    assert_eq!(pools.stats().allocated_bytes, baseline);
}

#[kithara::test(native, tokio)]
async fn decoder_readers_have_isolated_construction_gates() {
    let control = Arc::new(TestControl::new(media_info(0)));
    let stream = match Stream::<TestStream>::new(TestConfig {
        source: TestSource::new(control),
    })
    .await
    {
        Ok(stream) => stream,
        Err(error) => panic!("test stream construction failed: {error}"),
    };
    let shared_stream = SharedStream::new(stream);
    let initial = shared_stream.open_initial_reader();
    let rebuild = shared_stream.open_rebuild_reader(0);
    let Some(initial_gate) = initial.construction_gate() else {
        panic!("initial reader must carry a construction gate");
    };
    let Some(rebuild_gate) = rebuild.construction_gate() else {
        panic!("rebuild reader must carry a construction gate");
    };

    initial_gate.arm();

    assert!(initial_gate.is_armed());
    assert!(!rebuild_gate.is_armed());
}

#[kithara::test(tokio)]
async fn matching_replacement_aborts_primed_incoming_before_profile_prepare(route_pcm: RoutePcm) {
    let RebuildFixture {
        control,
        drops,
        pools,
        mut source,
    } = test_source(1).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    control.set_exact_plan(plan);
    assert!(
        source
            .decode
            .begin_incoming(transition, OutgoingFrontier::Awaiting)
            .is_none()
    );
    assert!(source.decode.incoming_is_preparing(transition));
    let incoming = route_generation(&route_pcm, &pools, 8, 1, drops.clone());
    assert!(
        source
            .decode
            .install_incoming(transition, incoming)
            .is_none()
    );
    assert!(source.decode.incoming_is_priming(transition));
    let factory_control = control.clone();
    let factory_drops = drops.clone();
    let factory_pools = pools.clone();
    source.factory = DecoderFactory::new(
        move |_reader, _info, rate| {
            assert_eq!(factory_control.aborted_transition(), Some(transition));
            assert_eq!(factory_drops.lock().as_slice(), &[8]);
            Ok(Box::new(RouteSignalDecoder::new(
                &route_pcm,
                2,
                rate.map_or(consts::SAMPLE_RATE, NonZeroU32::get),
                None,
                None,
                factory_drops.clone(),
                factory_pools.clone(),
            )))
        },
        None,
    );
    enter_rebuilding(&mut source, recreate_state(1));

    assert_eq!(source.decode.incoming_transition(), None);
    assert_eq!(control.aborted_transition(), Some(transition));
    assert_eq!(drops.lock().as_slice(), &[8, 1]);
    assert_replacement_decodes(&mut source);
    source.finish_deferred();
    assert_eq!(drops.lock().as_slice(), &[8, 1]);
}

#[kithara::test(tokio)]
async fn replacement_aborts_building_incoming_and_retires_its_late_completion(route_pcm: RoutePcm) {
    let RebuildFixture {
        control,
        drops,
        pools,
        mut source,
    } = test_source(1).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    control.set_exact_plan(plan);
    assert!(
        source
            .decode
            .begin_incoming(transition, OutgoingFrontier::Awaiting)
            .is_none()
    );
    assert!(source.decode.incoming_is_preparing(transition));
    let incoming = route_generation(&route_pcm, &pools, 8, 1, drops.clone());

    install_route_factory(&route_pcm, &pools, &mut source, 2, drops.clone());
    enter_rebuilding(&mut source, recreate_state(1));
    let rejected = source.decode.install_incoming(transition, incoming);
    assert!(rejected.is_some());
    drop(rejected);
    assert_eq!(source.decode.incoming_transition(), None);
    assert_eq!(control.aborted_transition(), Some(transition));
    assert_eq!(drops.lock().as_slice(), &[1, 8]);
    assert_replacement_decodes(&mut source);
    source.finish_deferred();
    assert_eq!(drops.lock().as_slice(), &[1, 8]);
}

#[kithara::test(tokio)]
async fn transition_wait_with_demand_in_flight_is_upstream_pending() {
    let RebuildFixture {
        control,
        mut source,
        ..
    } = test_source(1).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    control.set_exact_plan(plan);
    assert!(
        source
            .decode
            .begin_incoming(transition, OutgoingFrontier::Awaiting)
            .is_none()
    );
    control.set_demand_in_flight(true);

    assert_eq!(
        source.transition_wait_reason(),
        WaitingReason::WaitingDemand
    );
}

#[kithara::test(tokio)]
async fn transition_wait_without_demand_stays_watchdog_visible() {
    let RebuildFixture {
        control,
        mut source,
        ..
    } = test_source(1).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    control.set_exact_plan(plan);
    assert!(
        source
            .decode
            .begin_incoming(transition, OutgoingFrontier::Awaiting)
            .is_none()
    );

    assert_eq!(source.transition_wait_reason(), WaitingReason::Waiting);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_pending_poll_blocks() {
    let RebuildFixture { mut source, .. } = test_source(1).await;
    let transition = exact_incoming_plan().transition();
    source
        .decode
        .begin_incoming(transition, OutgoingFrontier::Awaiting);
    assert!(matches!(
        source.step_track(),
        TrackStep::Blocked(WaitingReason::Waiting)
    ));
    assert!(source.decode.incoming_is_preparing(transition));
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_completion_waits_for_shell_routing() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
    assert!(drops.lock().is_empty());
    install_test_factory(&mut source, 2, drops.clone());
    enter_rebuilding(&mut source, recreate_state(1));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
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
    source.finish_deferred();
    assert_eq!(drops.lock().as_slice(), &[1]);
}

#[kithara::test(tokio)]
async fn rebuild_prepares_generation_profiles_before_rt_install() {
    let RebuildFixture { mut source, .. } =
        test_source_with_mode(1, GaplessMode::SilenceTrim(SilenceTrimParams::default())).await;
    let profile_reads = Arc::new(AtomicU64::new(0));
    let factory_reads = profile_reads.clone();
    source.factory = DecoderFactory::new(
        move |_reader, _info, _rate| {
            Ok(Box::new(ProfileCountingDecoder {
                gapless_profile_reads: factory_reads.clone(),
            }))
        },
        None,
    );
    assert_eq!(profile_reads.load(Ordering::Acquire), 0);
    enter_rebuilding(&mut source, recreate_state(1));
    assert_eq!(profile_reads.load(Ordering::Acquire), 1);
    source.finish_deferred();
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(profile_reads.load(Ordering::Acquire), 1);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_completion_installs_once() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    install_test_factory(&mut source, 2, drops.clone());
    enter_rebuilding(&mut source, recreate_state(1));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(1)
    );
    assert!(source.decode.incoming_transition().is_none());
    assert_eq!(drops.lock().as_slice(), &[1]);
    source.finish_deferred();
    assert_eq!(drops.lock().as_slice(), &[1]);
}

#[kithara::test(tokio)]
async fn format_boundary_rebuild_rebases_decode_head_to_rendered_source(route_pcm: RoutePcm) {
    let RouteFixture {
        control,
        drops,
        pools,
        mut source,
        ..
    } = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let mut route_recreated = false;
    let chunk = next_decoded_chunk(&mut source, &mut route_recreated);

    let raw = source
        .resume
        .decode_head()
        .expect("decoded chunk must advance the raw head");
    let rendered_frame = chunk
        .meta
        .frame_offset
        .saturating_add(u64::from(chunk.meta.frames / 2));
    let rendered = (rendered_frame, chunk.meta.spec.sample_rate.get());
    source.commit_source_end(
        SourceEnd::new(rendered_frame, chunk.meta.spec.sample_rate),
        chunk.meta,
    );
    assert!(
        raw.0 > rendered.0,
        "fixture requires raw PCM ahead of output"
    );

    let landing = spec(rendered.1)
        .duration_for(rendered.0)
        .expect("rendered position");
    control.set_media_info(media_info(1));
    install_route_factory(&route_pcm, &pools, &mut source, 2, drops);
    source
        .install_replacement(
            recreate_state(1),
            Some(SourceEnd::new(rendered_frame, chunk.meta.spec.sample_rate)),
        )
        .expect("replacement at rendered frontier");
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert_eq!(source.resume.decode_head(), Some(rendered));
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(1)
    );
    control.set_exact_plan(exact_incoming_plan());
    source.prepare_deferred();
    source.finish_deferred();
    assert_eq!(
        control.landing(),
        Some(
            spec(rendered.1)
                .duration_for(rendered.0)
                .expect("rendered fixture landing fits Duration"),
        ),
        "the next ABR plan must start from the rebuilt rendered frontier"
    );
    let mut rebuilt = false;
    let chunk = next_decoded_chunk(&mut source, &mut rebuilt);
    assert_eq!(chunk.meta.timestamp, landing);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_completion_emits_decoder_changed_cause() {
    let RebuildFixture { mut source, .. } = test_source(1).await;
    let bus = EventBus::new(16);
    let mut events = bus.subscribe();
    source.emit = Arc::new(DeferredBus::new(bus, 16));
    enter_rebuilding(&mut source, recreate_state(1));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Decoding
    ));
    assert!(events.try_recv().is_err());
    source.finish_deferred();
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(AudioLaneEvent::Decoder(DecoderEvent::DecoderChanged {
            cause: DecoderChangeCause::FormatBoundary,
            ..
        }))
    ));
}

#[kithara::test(tokio)]
async fn decode_error_precedes_track_failure_on_event_bus() {
    let RebuildFixture { mut source, .. } = test_source(0).await;
    let bus = EventBus::new(16);
    let mut events = bus.subscribe();
    source.emit = Arc::new(DeferredBus::new(bus, 16));
    let replacement = DecoderGeneration::new(
        Box::new(FailingDecoder),
        Some(media_info(0)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    );
    let old = source.decode.replace_active(replacement);
    drop(old);

    assert!(matches!(source.step_track(), TrackStep::Failed(_)));
    assert!(events.try_recv().is_err());
    source.finish_deferred();

    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(AudioLaneEvent::Decoder(DecoderEvent::DecodeError {
            detail: "fixture decode failure",
            ..
        }))
    ));
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(AudioLaneEvent::Audio(AudioEvent::TrackFailed {
            failure: TrackFailureKind::Decode {
                kind: crate::DecodeErrorKind::InvalidData
            },
        }))
    ));
}
