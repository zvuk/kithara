use super::*;

pub(in crate::pipeline::source) async fn route_signal_source(
    route_pcm: &RoutePcm,
    initial_host_rate: u32,
) -> RouteFixture {
    route_source(
        route_pcm,
        RouteParams {
            initial_host_rate,
            chunks_before_eof: None,
            gapless: None,
            incoming_chunks_before_eof: None,
            active_timeline_gap: 0,
            incoming_timeline_gap: 0,
            segmented: false,
        },
    )
    .await
}

pub(in crate::pipeline::source) async fn route_signal_source_with_eof(
    route_pcm: &RoutePcm,
    initial_host_rate: u32,
    chunks_before_eof: usize,
) -> RouteFixture {
    route_source(
        route_pcm,
        RouteParams {
            initial_host_rate,
            chunks_before_eof: Some(chunks_before_eof),
            gapless: None,
            incoming_chunks_before_eof: None,
            active_timeline_gap: 0,
            incoming_timeline_gap: 0,
            segmented: false,
        },
    )
    .await
}

pub(in crate::pipeline::source) async fn route_signal_source_with_gapless(
    route_pcm: &RoutePcm,
    initial_host_rate: u32,
    gapless: GaplessInfo,
) -> RouteFixture {
    route_source(
        route_pcm,
        RouteParams {
            initial_host_rate,
            chunks_before_eof: None,
            gapless: Some(gapless),
            incoming_chunks_before_eof: None,
            active_timeline_gap: 0,
            incoming_timeline_gap: 0,
            segmented: false,
        },
    )
    .await
}

pub(in crate::pipeline::source) async fn route_signal_source_with_gapless_eof(
    route_pcm: &RoutePcm,
    initial_host_rate: u32,
    gapless: GaplessInfo,
    chunks_before_eof: usize,
) -> RouteFixture {
    route_source(
        route_pcm,
        RouteParams {
            initial_host_rate,
            chunks_before_eof: Some(chunks_before_eof),
            gapless: Some(gapless),
            incoming_chunks_before_eof: None,
            active_timeline_gap: 0,
            incoming_timeline_gap: 0,
            segmented: false,
        },
    )
    .await
}

/// Both sides of a transition run out of source, on the same media length.
///
/// Two variants of one track end together, so an incoming that lands at the container origin
/// stages to the very frame the outgoing frontier stops at. Which decoder reports its exhaustion
/// first is then a race, and this fixture pins the order the race can take.
pub(in crate::pipeline::source) async fn route_signal_source_with_finite_sides(
    route_pcm: &RoutePcm,
    initial_host_rate: u32,
    chunks_before_eof: usize,
    incoming_chunks_before_eof: usize,
) -> RouteFixture {
    route_source(
        route_pcm,
        RouteParams {
            initial_host_rate,
            chunks_before_eof: Some(chunks_before_eof),
            gapless: None,
            incoming_chunks_before_eof: Some(incoming_chunks_before_eof),
            active_timeline_gap: 0,
            incoming_timeline_gap: 0,
            segmented: false,
        },
    )
    .await
}

pub(in crate::pipeline::source) async fn route_signal_source_with_finite_incoming(
    route_pcm: &RoutePcm,
    initial_host_rate: u32,
    incoming_chunks_before_eof: usize,
) -> RouteFixture {
    route_source(
        route_pcm,
        RouteParams {
            initial_host_rate,
            chunks_before_eof: None,
            gapless: None,
            incoming_chunks_before_eof: Some(incoming_chunks_before_eof),
            active_timeline_gap: 0,
            incoming_timeline_gap: 0,
            segmented: false,
        },
    )
    .await
}

pub(in crate::pipeline::source) async fn route_source(
    route_pcm: &RoutePcm,
    params: RouteParams,
) -> RouteFixture {
    let pools = pools();
    let control = Arc::new(TestControl::new(media_info(0)));
    let drops = Arc::new(Mutex::new(Vec::new()));
    let chunks_before_eof = params.chunks_before_eof;
    let gapless = params.gapless;
    let incoming_chunks_before_eof = params.incoming_chunks_before_eof;
    let active_timeline_gap = params.active_timeline_gap;
    let incoming_timeline_gap = params.incoming_timeline_gap;
    let segmented = params.segmented;
    let test_source = if segmented {
        TestSource::segmented(control.clone())
    } else {
        TestSource::new(control.clone())
    };
    let phase = test_source.phase_handle();
    let stream = match Stream::<TestStream>::new(TestConfig {
        source: test_source,
    })
    .await
    {
        Ok(stream) => stream,
        Err(err) => panic!("test stream construction failed: {err}"),
    };
    let shared_stream = SharedStream::new(stream);
    let container_byte_len = shared_stream.len();
    let factory_drops = drops.clone();
    let factory_pools = pools.clone();
    let factory_pcm = route_pcm.clone();
    let decoder_factory = DecoderFactory::new(
        move |reader, _info, host_rate| {
            if segmented && reader.byte_len() != container_byte_len {
                return Err(DecodeError::InvalidData {
                    detail: "init-bearing container demuxed from a media byte",
                });
            }
            let rate = host_rate.map_or(consts::SAMPLE_RATE, NonZeroU32::get);
            Ok(Box::new(
                RouteSignalDecoder::new(
                    &factory_pcm,
                    99,
                    rate,
                    gapless,
                    incoming_chunks_before_eof,
                    factory_drops.clone(),
                    factory_pools.clone(),
                )
                .with_timeline_gap(incoming_timeline_gap),
            ))
        },
        None,
    );
    let mode = if gapless.is_some() {
        GaplessMode::MediaOnly
    } else {
        GaplessMode::Disabled
    };
    let mut decoder = RouteSignalDecoder::new(
        route_pcm,
        1,
        consts::SAMPLE_RATE,
        gapless,
        chunks_before_eof,
        drops.clone(),
        pools.clone(),
    )
    .with_timeline_gap(active_timeline_gap);
    decoder.phase = Some(phase.clone());
    let decode = ActiveDecode::new(
        DecoderGeneration::new(Box::new(decoder), Some(media_info(0)), 0, None, None, mode),
        mode,
        None,
        &pools,
    )
    .expect("decode scratch fits pools");
    let source = StreamAudioSource::new(
        shared_stream,
        decode,
        SourceDecoderConfig {
            factory: decoder_factory,
            host_rate: NonZeroU32::new(params.initial_host_rate),
            backend: kithara_decode::DecoderBackend::default(),
            playback_resampler_backend: "none",
        },
        Arc::new(DeferredBus::new(EventBus::default(), 16)),
        Arc::new(NoopWorkerWake),
    );
    RouteFixture {
        control,
        drops,
        phase,
        pools,
        source,
    }
}

pub(in crate::pipeline::source) async fn route_signal_source_with_gaps(
    route_pcm: &RoutePcm,
    active_timeline_gap: u64,
    incoming_timeline_gap: u64,
) -> RouteFixture {
    route_source(
        route_pcm,
        RouteParams {
            active_timeline_gap,
            incoming_timeline_gap,
            chunks_before_eof: None,
            gapless: None,
            incoming_chunks_before_eof: None,
            initial_host_rate: consts::SAMPLE_RATE,
            segmented: false,
        },
    )
    .await
}

pub(in crate::pipeline::source) fn run_pending_rebuild_inline(
    source: &mut StreamAudioSource<TestStream>,
) {
    source.prepare_deferred();
    source.finish_deferred();
}

pub(in crate::pipeline::source) fn append_left_channel(left: &mut Vec<f32>, chunk: &AudioChunk) {
    let channels = usize::from(chunk.meta.spec.channels);
    for frame in 0..chunk.frames() {
        left.push(chunk.samples[frame * channels]);
    }
}

pub(in crate::pipeline::source) fn peak_first_diff(
    left: &[f32],
    center: usize,
    half: usize,
) -> f32 {
    assert!(
        (1..left.len()).contains(&center),
        "first-difference center must be in 1..{}, got {center}",
        left.len(),
    );
    let start = center.saturating_sub(half).max(1);
    let end = center.saturating_add(half).min(left.len() - 1);
    let mut peak = 0.0_f32;
    for i in start..=end {
        peak = peak.max((left[i] - left[i - 1]).abs());
    }
    peak
}

pub(in crate::pipeline::source) fn next_test_chunk(
    source: &mut StreamAudioSource<TestStream>,
    route_recreated: &mut bool,
) -> AudioChunk {
    let chunk = next_decoded_chunk(source, route_recreated);
    source.commit_source_end(
        SourceEnd::new(
            chunk
                .meta
                .frame_offset
                .saturating_add(u64::from(chunk.meta.frames)),
            chunk.meta.spec.sample_rate,
        ),
        chunk.meta,
    );
    chunk
}

pub(in crate::pipeline::source) fn next_decoded_chunk(
    source: &mut StreamAudioSource<TestStream>,
    route_recreated: &mut bool,
) -> AudioChunk {
    loop {
        run_pending_rebuild_inline(source);
        *route_recreated |=
            source.decode.output_spec().sample_rate.get() == consts::ROUTE_SAMPLE_RATE;
        match source.step_track() {
            TrackStep::Produced(fetch) => return produced_data(fetch),
            TrackStep::StateChanged => {
                *route_recreated |=
                    source.decode.output_spec().sample_rate.get() != consts::SAMPLE_RATE;
            }
            TrackStep::Blocked(_) => {}
            TrackStep::Eof => panic!("route test source reached EOF"),
            TrackStep::Failed(_) => panic!("route test source failed"),
        }
    }
}

pub(in crate::pipeline::source) fn enter_rebuilding(
    source: &mut StreamAudioSource<TestStream>,
    recreate: RecreateState,
) {
    source
        .install_replacement(recreate, None)
        .expect("synchronous replacement");
}

pub(in crate::pipeline::source) fn install_test_factory(
    source: &mut StreamAudioSource<TestStream>,
    id: u64,
    drops: Arc<Mutex<Vec<u64>>>,
) {
    source.factory = DecoderFactory::new(
        move |_reader, _info, _rate| Ok(Box::new(TestDecoder::new(id, drops.clone()))),
        None,
    );
}

pub(in crate::pipeline::source) fn exact_incoming_plan() -> VariantReaderPlan {
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    let claim = abr
        .claim_pending_decision(VariantIndex::new(0))
        .expect("incoming rebuild fixture requires an exact ABR claim");
    let transition = VariantTransition::new(
        VariantTransitionId::new(claim.ticket()),
        VariantIndex::new(0),
        VariantIndex::new(1),
    );
    VariantReaderPlan::new(transition, media_info(1), Duration::ZERO)
}

pub(in crate::pipeline::source) fn route_generation(
    route_pcm: &RoutePcm,
    pools: &Pools,
    decoder_id: u64,
    variant: u32,
    drops: Arc<Mutex<Vec<u64>>>,
) -> DecoderGeneration {
    DecoderGeneration::new(
        Box::new(RouteSignalDecoder::new(
            route_pcm,
            decoder_id,
            consts::SAMPLE_RATE,
            None,
            None,
            drops,
            pools.clone(),
        )),
        Some(media_info(variant)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    )
}

pub(in crate::pipeline::source) fn install_route_factory(
    route_pcm: &RoutePcm,
    pools: &Pools,
    source: &mut StreamAudioSource<TestStream>,
    id: u64,
    drops: Arc<Mutex<Vec<u64>>>,
) {
    let pcm = route_pcm.clone();
    let pools = pools.clone();
    source.factory = DecoderFactory::new(
        move |_reader, _info, rate| {
            Ok(Box::new(RouteSignalDecoder::new(
                &pcm,
                id,
                rate.map_or(consts::SAMPLE_RATE, NonZeroU32::get),
                None,
                None,
                drops.clone(),
                pools.clone(),
            )))
        },
        None,
    );
}

pub(in crate::pipeline::source) fn assert_replacement_decodes(
    source: &mut StreamAudioSource<TestStream>,
) {
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
    assert!(matches!(source.step_track(), TrackStep::Produced(_)));
}
