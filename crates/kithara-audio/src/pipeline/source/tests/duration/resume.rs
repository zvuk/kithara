use super::fixtures::*;

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn promote_before_output(
    source: &mut StreamAudioSource<TestStream>,
    control: &TestControl,
    mut outgoing: AudioChunkInfo,
) -> AudioChunkInfo {
    loop {
        hang_tick!();
        source.prepare_deferred();
        if control.promote_calls() > 0 {
            return outgoing;
        }
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                outgoing = produced_data(fetch).meta;
                source
                    .playhead
                    .advance(&crate::audio::chunk_position(&outgoing));
                hang_reset!();
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => source.finish_deferred(),
            TrackStep::Eof => panic!("the prepared incoming must promote before EOF"),
            TrackStep::Failed(failure) => panic!("physical promotion failed: {failure:?}"),
        }
    }
}

#[kithara::test(tokio)]
#[case::descending(700, 0, true)]
#[case::ascending(0, 700, true)]
#[case::unknown_incoming(700, 0, false)]
async fn host_rate_after_promotion_resumes_in_the_incoming_clock_before_output(
    #[case] outgoing_origin: u64,
    #[case] incoming_origin: u64,
    #[case] incoming_reports_origin: bool,
) {
    let outgoing = extent(Some(Duration::from_secs(1)), outgoing_origin, true);
    let incoming = extent(
        Some(Duration::from_secs(1)),
        incoming_origin,
        incoming_reports_origin,
    );
    let (mut source, control) = physical_fixture(outgoing, GaplessMode::MediaOnly).await;
    let original = first_chunk(&mut source);
    source
        .playhead
        .advance(&crate::audio::chunk_position(&original.meta));
    source.commit_source_end(
        crate::SourceEnd::new(original.meta.frame_range().end, original.spec().sample_rate),
        original.meta,
    );
    start_physical_promotion(&mut source, &control, incoming, Duration::ZERO);
    let last_outgoing = promote_before_output(&mut source, &control, original.meta);
    let revision = source
        .discontinuity()
        .expect("incoming revision")
        .revision();
    assert_ne!(revision, last_outgoing.source_revision);
    let expected = last_outgoing.end_timestamp - Duration::from_millis(outgoing_origin + 17)
        + Duration::from_millis(incoming_origin + 17);
    let resume = source
        .resume
        .source_end()
        .expect("current-coordinate resume cut");
    assert_eq!(resume.sample_rate().get(), 1_000);
    assert_eq!(
        resume.frame(),
        u64::try_from(expected.as_millis()).expect("fixture timestamp fits u64")
    );
    assert_eq!(source.resume.decoded_end(revision), None);
    assert_eq!(source.playhead.position(), last_outgoing.end_timestamp);
    let expected_origin =
        incoming_reports_origin.then(|| Duration::from_millis(incoming_origin + 17));
    assert_eq!(source.decode.active().visible_origin(), expected_origin);

    source.commit_source_end(
        crate::SourceEnd::new(
            last_outgoing.frame_range().end,
            last_outgoing.spec.sample_rate,
        ),
        last_outgoing,
    );
    assert_eq!(
        source.resume.source_end(),
        Some(resume),
        "old buffered admission cannot restore its clock"
    );
    source.set_host_sample_rate(NonZeroU32::new(2_000).expect("new host rate"));
    let resumed = first_chunk(&mut source);
    assert_eq!(resumed.meta.timestamp, expected);
    assert_eq!(resumed.meta.visible_origin, expected_origin);
    assert_eq!(resumed.spec().sample_rate.get(), 2_000);
    let span = resumed
        .meta
        .source_span
        .expect("route recreation preserves incoming Source basis");
    assert_eq!(span.start(), resume.frame());
    assert_eq!(span.sample_rate(), resume.sample_rate());
    assert_eq!(span.mapping_revision(), resume.mapping_revision());
    let content_frame =
        u64::try_from(expected.as_millis()).expect("fixture timestamp fits u64") - incoming_origin;
    assert_eq!(
        &resumed.samples[..2],
        [f32::from(u16::try_from(content_frame).expect("fixture marker fits u16")) / 10_000.0; 2]
    );
    drain(&mut source, incoming);
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
}

#[kithara::test(tokio)]
async fn same_coordinate_promotion_retains_the_final_admitted_frontier() {
    let extent = extent(Some(Duration::from_secs(1)), 0, true);
    let (mut source, control) = physical_fixture(extent, GaplessMode::MediaOnly).await;
    let original = first_chunk(&mut source);
    let admitted = crate::SourceEnd::new(
        original.meta.frame_offset + u64::from(original.meta.frames / 2),
        original.spec().sample_rate,
    );
    source.commit_source_end(admitted, original.meta);
    source
        .playhead
        .advance(&crate::audio::chunk_position(&original.meta));
    start_physical_promotion(&mut source, &control, extent, Duration::ZERO);
    let last_outgoing = promote_before_output(&mut source, &control, original.meta);
    assert_eq!(
        source
            .discontinuity()
            .expect("same raw geometry")
            .revision(),
        last_outgoing.source_revision
    );
    assert!(last_outgoing.frame_range().end > admitted.frame());
    assert_eq!(source.resume.source_end(), Some(admitted));
    assert_eq!(
        source.resume.decoded_end(last_outgoing.source_revision),
        None
    );
    source.set_host_sample_rate(NonZeroU32::new(2_000).expect("new host rate"));
    let resumed = first_chunk(&mut source);
    assert_eq!(
        resumed.meta.timestamp,
        Duration::from_millis(admitted.frame())
    );
    assert_eq!(
        &resumed.samples[..2],
        [f32::from(u16::try_from(admitted.frame()).expect("fixture marker fits u16")) / 10_000.0;
            2]
    );
    assert_eq!(
        resumed
            .meta
            .source_span
            .expect("admitted Source basis")
            .start(),
        admitted.frame()
    );
}

#[kithara::test(tokio)]
async fn empty_promoted_tail_keeps_its_own_resume_endpoint_without_refining_duration() {
    let mut outgoing = extent(Some(Duration::from_secs(60)), 700, true);
    outgoing.media_origin = Some(Duration::ZERO);
    outgoing.frames = 64;
    let mut incoming = outgoing;
    incoming.origin = 0;
    let (mut source, control) = physical_fixture(outgoing, GaplessMode::MediaOnly).await;
    let original = first_chunk(&mut source);
    source
        .playhead
        .advance(&crate::audio::chunk_position(&original.meta));
    let declared = source.playhead.duration();
    start_physical_promotion(&mut source, &control, incoming, Duration::ZERO);
    promote_before_output(&mut source, &control, original.meta);
    let resume = source
        .resume
        .source_end()
        .expect("matching empty-tail endpoint");
    assert_eq!(resume.frame(), 33);
    assert_eq!(resume.sample_rate().get(), 1_000);
    assert_eq!(
        source.resume.decoded_end(
            source
                .discontinuity()
                .expect("incoming revision")
                .revision()
        ),
        None
    );
    source.set_host_sample_rate(NonZeroU32::new(2_000).expect("new host rate"));
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::Changing(_)
    ));
    assert_eq!(source.resume.source_end(), Some(resume));
    assert_eq!(source.playhead.duration(), declared);
    assert!(drain(&mut source, incoming).is_empty());
    assert!(matches!(
        source.phase,
        crate::pipeline::source::core::OwnerPhase::AtEof
    ));
    assert_eq!(
        source.decode.active().visible_origin(),
        Some(Duration::from_millis(17))
    );
    assert_eq!(source.playhead.duration(), declared);
    assert_eq!(source.media_end(), Some(Duration::from_millis(59_969)));
    assert_ne!(source.media_end(), Some(original.meta.end_timestamp));
}

#[kithara::test(tokio)]
async fn decoded_only_resume_preserves_the_real_source_mapping() {
    let extent = extent(Some(Duration::from_secs(1)), 700, true);
    let mut source = physical_source(extent).await;
    let original = first_chunk(&mut source);
    let landing =
        crate::SourceEnd::new(original.meta.frame_range().end, original.spec().sample_rate)
            .with_mapping_revision(std::num::NonZeroU64::new(17));
    source.commit_source_end(landing, original.meta);
    source.set_host_sample_rate(NonZeroU32::new(2_000).expect("new host rate"));
    let mapped = first_chunk(&mut source);
    let span = mapped
        .meta
        .source_span
        .expect("real route-recreated mapping");
    let mut cursor = crate::pipeline::decode::resume::ResumeCursor::default();
    cursor.record(
        &mapped,
        source
            .native_pcm_basis()
            .expect("actual pre-rebase PCM facts")
            .end_timestamp,
    );
    assert_eq!(
        cursor.decode_head(),
        Some((mapped.meta.frame_range().end, 2_000))
    );
    assert_eq!(
        cursor.source_end(),
        Some(
            crate::SourceEnd::new(span.end(), span.sample_rate())
                .with_mapping_revision(span.mapping_revision())
        )
    );
    assert_eq!(span.sample_rate().get(), 1_000);
    assert_eq!(span.mapping_revision(), landing.mapping_revision());
}
