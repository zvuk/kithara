use super::fixtures::*;

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
#[case::descending(700, 0)]
#[case::ascending(0, 700)]
async fn promoted_known_raw_clocks_keep_full_visible_length_and_seek_the_actual_tail(
    #[case] outgoing_origin: u64,
    #[case] incoming_origin: u64,
) {
    let outgoing = extent(Some(Duration::from_secs(1)), outgoing_origin, true);
    let incoming = extent(Some(Duration::from_secs(1)), incoming_origin, true);
    let (mut source, control) = physical_fixture(outgoing, GaplessMode::MediaOnly).await;
    let original = first_chunk(&mut source);
    assert_eq!(
        original.meta.visible_origin,
        Some(Duration::from_millis(outgoing_origin + 17))
    );
    let mut outgoing_end = original.meta.end_timestamp;
    let old_revision = original.meta.source_revision;
    start_physical_promotion(&mut source, &control, incoming, Duration::ZERO);
    let first_incoming = loop {
        hang_tick!();
        source.prepare_deferred();
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                if control.promote_calls() > 0 {
                    break chunk;
                }
                outgoing_end = chunk.meta.end_timestamp;
                assert_eq!(chunk.meta.visible_origin, original.meta.visible_origin);
                hang_reset!();
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => source.finish_deferred(),
            TrackStep::Eof => panic!("promotion must expose the actual incoming tail"),
            TrackStep::Failed(failure) => panic!("physical promotion failed: {failure:?}"),
        }
    };
    let expected_origin = Duration::from_millis(incoming_origin + 17);
    assert_eq!(first_incoming.meta.visible_origin, Some(expected_origin));
    assert_ne!(first_incoming.meta.source_revision, old_revision);
    assert_eq!(
        first_incoming.meta.timestamp - expected_origin,
        outgoing_end - original.meta.visible_origin.expect("original origin")
    );
    for (offset, &samples) in first_incoming.samples.as_chunks::<2>().0.iter().enumerate() {
        let absolute = first_incoming.meta.frame_offset + offset as u64;
        assert_eq!(
            samples,
            [f32::from(u16::try_from(absolute - incoming_origin).expect("fixture marker fits u16"))
                / 10_000.0; 2]
        );
    }
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    let chunks = drain(&mut source, incoming);
    let end = chunks.last().map_or_else(
        || first_incoming.meta.frame_range().end,
        |meta| meta.frame_range().end,
    );
    assert_eq!(end, incoming_origin + 969);
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    let target = Duration::from_millis(incoming_origin + 960);
    assert!(
        matches!(source.seek(target).expect("raw seek within promoted tail"),
        crate::SeekOutcome::Landed { target: asked, landed_at } if asked == target && landed_at == target)
    );
    let replay = drain(&mut source, incoming);
    assert_eq!(
        replay
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        9
    );
    assert!(
        replay
            .iter()
            .all(|meta| meta.visible_origin == Some(expected_origin))
    );
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn promoted_unknown_original_geometry_never_adopts_a_sought_suffix() {
    for incoming_origin in [700, 0] {
        let outgoing = extent(Some(Duration::from_secs(1)), 700, true);
        let mut incoming = extent(Some(Duration::from_secs(1)), incoming_origin, false);
        incoming.gapless = outgoing.gapless;
        let (mut source, control) = physical_fixture(outgoing, GaplessMode::MediaOnly).await;
        let original = first_chunk(&mut source);
        let initial = source.playhead.duration();
        start_physical_promotion(&mut source, &control, incoming, Duration::from_millis(200));
        let mut incoming_chunks = Vec::new();
        loop {
            hang_tick!();
            source.prepare_deferred();
            match source.step_track() {
                TrackStep::Produced(fetch) => {
                    let chunk = produced_data(fetch);
                    if control.promote_calls() > 0 {
                        assert_eq!(chunk.meta.visible_origin, None);
                        assert_ne!(chunk.meta.source_revision, original.meta.source_revision);
                        assert!(chunk.samples.iter().all(|sample| sample.is_finite()));
                        incoming_chunks.push(chunk.meta);
                    }
                    hang_reset!();
                }
                TrackStep::Eof => break,
                TrackStep::StateChanged | TrackStep::Blocked(_) => source.finish_deferred(),
                TrackStep::Failed(failure) => {
                    panic!("unknown geometry does not veto transport: {failure:?}")
                }
            }
        }
        assert!(!incoming_chunks.is_empty());
        assert_eq!(source.decode.active().visible_origin(), None);
        assert_eq!(source.decode.active().comparison_base(), None);
        assert_eq!(source.playhead.duration(), initial);
        assert!(matches!(
            source
                .seek(Duration::from_millis(incoming_origin + 960))
                .expect("valid raw seek remains allowed"),
            crate::SeekOutcome::Landed { .. }
        ));
    }
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
#[case::shifted_clock(700, 0)]
#[case::shared_clock(0, 0)]
async fn a_landed_incoming_without_its_own_start_carries_the_track_visible_origin(
    #[case] outgoing_origin: u64,
    #[case] incoming_origin: u64,
) {
    let outgoing = extent(Some(Duration::from_secs(1)), outgoing_origin, true);
    let incoming = extent(Some(Duration::from_secs(1)), incoming_origin, true);
    let (mut source, control) = physical_fixture(outgoing, GaplessMode::MediaOnly).await;
    let original = first_chunk(&mut source);
    let outgoing_visible = original
        .meta
        .visible_origin
        .expect("observed outgoing origin");
    let mut outgoing_end = original.meta.end_timestamp;
    start_physical_promotion(&mut source, &control, incoming, Duration::from_millis(200));
    let first_incoming = loop {
        hang_tick!();
        source.prepare_deferred();
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                if control.promote_calls() > 0 {
                    break chunk;
                }
                outgoing_end = chunk.meta.end_timestamp;
                hang_reset!();
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => source.finish_deferred(),
            TrackStep::Eof => panic!("promotion must expose the incoming tail"),
            TrackStep::Failed(failure) => panic!("landed promotion failed: {failure:?}"),
        }
    };
    let carried = Duration::from_millis(incoming_origin + 17);
    assert_eq!(first_incoming.meta.visible_origin, Some(carried));
    assert_eq!(source.decode.active().visible_origin(), Some(carried));
    assert_eq!(
        source.decode.active().comparison_base(),
        Some(Duration::from_millis(incoming_origin))
    );
    assert_eq!(
        first_incoming.meta.timestamp - carried,
        outgoing_end - outgoing_visible,
        "the visible timeline continues across the splice"
    );
}

/// AAC without gapless metadata keeps its default priming visible while content
/// alignment skips it; a variant without priming starts its content at its own zero.
#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
#[case::landed(false)]
#[case::observed(true)]
async fn an_incoming_with_other_priming_reports_its_own_visible_origin(
    #[case] incoming_reports_origin: bool,
) {
    let mut outgoing = extent(Some(Duration::from_secs(1)), 0, true);
    outgoing.gapless = None;
    outgoing.priming = Some(23);
    let mut incoming = extent(Some(Duration::from_secs(1)), 0, incoming_reports_origin);
    incoming.gapless = None;
    incoming.priming = Some(0);
    let (mut source, control) = physical_fixture(outgoing, GaplessMode::MediaOnly).await;
    let original = first_chunk(&mut source);
    assert_eq!(original.meta.visible_origin, Some(Duration::ZERO));
    start_physical_promotion(&mut source, &control, incoming, Duration::from_millis(200));
    let first_incoming = loop {
        hang_tick!();
        source.prepare_deferred();
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                if control.promote_calls() > 0 {
                    break chunk;
                }
                hang_reset!();
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => source.finish_deferred(),
            TrackStep::Eof => panic!("promotion must expose the incoming tail"),
            TrackStep::Failed(failure) => panic!("landed promotion failed: {failure:?}"),
        }
    };
    let visible = incoming_reports_origin.then_some(Duration::ZERO);
    assert_eq!(first_incoming.meta.visible_origin, visible);
    assert_eq!(source.decode.active().visible_origin(), visible);
    assert_eq!(source.decode.active().comparison_base(), visible);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn a_finished_empty_incoming_tail_does_not_refine_from_the_outgoing_endpoint() {
    let mut outgoing = extent(Some(Duration::from_secs(60)), 700, true);
    outgoing.media_origin = Some(Duration::ZERO);
    outgoing.frames = 64;
    let mut incoming = outgoing;
    incoming.origin = 0;
    let (mut source, control) = physical_fixture(outgoing, GaplessMode::MediaOnly).await;
    let original = first_chunk(&mut source);
    assert_eq!(original.frames(), 16);
    assert!(source.decode.active().is_source_exhausted());
    let declared = source.playhead.duration();
    start_physical_promotion(&mut source, &control, incoming, Duration::ZERO);
    loop {
        hang_tick!();
        source.prepare_deferred();
        match source.step_track() {
            TrackStep::Eof => break,
            TrackStep::StateChanged | TrackStep::Blocked(_) => source.finish_deferred(),
            TrackStep::Produced(_) => {
                panic!("matching finished incoming has no frames beyond the cut")
            }
            TrackStep::Failed(failure) => panic!("empty tail must end normally: {failure:?}"),
        }
    }
    assert!(control.promote_calls() > 0);
    assert_eq!(
        source.decode.active().visible_origin(),
        Some(Duration::from_millis(17))
    );
    assert_eq!(source.playhead.duration(), declared);
    assert_ne!(declared, Some(Duration::from_millis(716)));
    let revision = source
        .discontinuity()
        .expect("incoming revision")
        .revision();
    assert_ne!(revision, original.meta.source_revision);
    assert_eq!(source.resume.decoded_end(revision), None);
    assert_eq!(source.media_end(), Some(Duration::from_millis(59_969)));
    assert_ne!(source.media_end(), Some(original.meta.end_timestamp));
}

#[kithara::test(tokio)]
async fn an_unrepresentable_known_origin_retires_the_incoming_without_model_retry() {
    use crate::pipeline::decode::transition::{IncomingPrime, OutgoingFrontier};
    let outgoing = extent(Some(Duration::from_secs(1)), 0, true);
    let (mut source, control) = physical_fixture(outgoing, GaplessMode::MediaOnly).await;
    let original = first_chunk(&mut source);
    start_physical_promotion(&mut source, &control, outgoing, Duration::ZERO);
    let transition = control
        .exact_plan
        .lock()
        .as_ref()
        .expect("exact plan")
        .transition();
    let rate = original.spec().sample_rate;
    let mut incoming = DecoderGeneration::new(
        Box::new(PhysicalDecoder::new(
            outgoing,
            rate,
            crate::test_pools::pools(),
        )),
        Some(media_info(1)),
        0,
        None,
        None,
        GaplessMode::MediaOnly,
    );
    incoming.retain_visible_origin(Some(Duration::MAX), None);
    assert_eq!(incoming.comparison_origin(GaplessMode::MediaOnly), None);
    drop(
        source
            .decode
            .begin_incoming(transition, OutgoingFrontier::Awaiting),
    );
    assert!(
        crate::pipeline::decode::transition_promotion::preparation::fixtures::install(
            &mut source.decode,
            transition,
            incoming
        )
        .is_none()
    );
    assert_eq!(
        source.decode.prime_incoming(OutgoingFrontier::Exact {
            frame: original.meta.frame_range().end,
            rate: rate.get(),
        }),
        IncomingPrime::Failed
    );
    assert!(!source.decode.has_live_incoming());
    source.retire_failed_incoming(control.as_ref());
    assert_eq!(control.aborted_transition(), Some(transition));
    assert_eq!(source.decode.incoming_transition(), None);
    let next = first_chunk(&mut source);
    assert!(next.frames() > 0);
    assert_eq!(next.meta.visible_origin, original.meta.visible_origin);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
#[case::same_rate_identity_mapping(1_000)]
#[case::host_rate_mapping(2_000)]
async fn promotion_seals_a_retained_source_mapping_before_native_incoming_pcm(
    #[case] host_rate: u32,
) {
    let extent = extent(Some(Duration::from_secs(1)), 0, false);
    let (mut source, control) = physical_fixture(extent, GaplessMode::MediaOnly).await;
    source
        .seek(Duration::from_millis(200))
        .expect("original-unobserved raw seek");
    let first = first_chunk(&mut source);
    assert_eq!(first.meta.visible_origin, None);
    let landing = crate::SourceEnd::new(first.meta.frame_range().end, first.spec().sample_rate);
    source.commit_source_end(landing, first.meta);
    if host_rate == 1_000 {
        source
            .install_replacement(
                RecreateState {
                    media_info: Some(media_info(0)),
                    offset: 0,
                    cause: RecreateCause::HostRateChange,
                },
                Some(crate::pipeline::seek::ResumeTarget::Source(landing)),
            )
            .expect("same-rate retained identity mapping");
    } else {
        source.set_host_sample_rate(NonZeroU32::new(host_rate).expect("host rate"));
    }
    let retained = first_chunk(&mut source);
    assert_eq!(retained.meta.visible_origin, None);
    assert_eq!(retained.spec().sample_rate.get(), host_rate);
    let span = retained
        .meta
        .source_span
        .expect("recreation preserves the original Source basis");
    assert_eq!(span.sample_rate(), first.spec().sample_rate);
    assert_eq!(span.position_at(0), Some(retained.meta.timestamp));
    let before = source.discontinuity().expect("retained geometry stamp");
    let last_retained_point = retained.meta.timestamp
        + retained
            .spec()
            .duration_for(u64::from(retained.meta.frames))
            .expect("actual extent");
    start_physical_promotion(&mut source, &control, extent, Duration::from_millis(200));
    let incoming = loop {
        hang_tick!();
        source.prepare_deferred();
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                if control.promote_calls() > 0 {
                    break chunk;
                }
                assert!(chunk.meta.source_span.is_some());
                assert_eq!(chunk.meta.source_revision, before.revision());
                hang_reset!();
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => source.finish_deferred(),
            TrackStep::Eof => panic!("incoming retains a real suffix"),
            TrackStep::Failed(failure) => panic!("source basis handoff failed: {failure:?}"),
        }
    };
    let after = source.discontinuity().expect("incoming geometry stamp");
    assert_eq!(
        before.spec(),
        after.spec(),
        "physical format stays identical"
    );
    assert_ne!(
        before.revision(),
        after.revision(),
        "Source mapping basis changes even at equal origin/spec"
    );
    assert_eq!(incoming.meta.visible_origin, None);
    assert_eq!(incoming.meta.source_revision, after.revision());
    assert_eq!(
        retained.meta.source_revision,
        before.revision(),
        "buffered metadata retains its epoch"
    );
    assert_eq!(
        incoming.meta.source_span, None,
        "native decoder PCM is not renumbered into retained Source"
    );
    control.set_media_info(
        source
            .decode
            .active()
            .media_info()
            .expect("promoted incoming metadata")
            .clone(),
    );
    assert!(incoming.meta.timestamp >= last_retained_point);
    for (offset, &samples) in incoming.samples.as_chunks::<2>().0.iter().enumerate() {
        let at = incoming.meta.frame_offset + offset as u64;
        let expected = f32::from(
            u16::try_from(at * 1_000 / u64::from(host_rate)).expect("fixture marker fits u16"),
        ) / 10_000.0;
        assert_eq!(samples, [expected; 2]);
    }
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    let chunks = drain(&mut source, extent);
    assert!(
        chunks
            .iter()
            .all(|meta| meta.source_revision == after.revision())
    );
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
}
