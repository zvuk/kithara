use super::fixtures::*;

#[kithara::test(tokio)]
#[case::unknown(None)]
#[case::overestimated(Some(Duration::from_secs(60)))]
async fn natural_eof_measures_trimmed_pcm_from_its_nonzero_original_origin(
    #[case] declared: Option<Duration>,
) {
    let mut extent = extent(declared, 700, false);
    extent.media_origin = Some(Duration::from_millis(700));
    let mut source = physical_source(extent).await;
    let chunks = drain(&mut source, extent);
    assert_eq!(
        chunks
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        952
    );
    assert_eq!(
        chunks.first().expect("visible head").timestamp,
        Duration::from_millis(717)
    );
    assert_eq!(
        chunks.last().expect("visible tail").frame_range().end,
        1_669
    );
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    assert_eq!(source.media_end(), Some(Duration::from_millis(969)));
    let landed = source
        .seek(Duration::from_millis(1_500))
        .expect("raw seek after genuine EOF");
    assert!(
        matches!(landed, crate::SeekOutcome::Landed { landed_at, .. }
        if landed_at == Duration::from_millis(1_500))
    );
    let replay = drain(&mut source, extent);
    assert_eq!(
        replay
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        169
    );
    assert_eq!(
        replay.first().expect("replay head").timestamp,
        Duration::from_millis(1_500)
    );
    assert_eq!(replay.last().expect("replay tail").frame_range().end, 1_669);
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    assert_eq!(source.media_end(), Some(Duration::from_millis(969)));
    let past = source
        .seek(Duration::from_millis(1_669))
        .expect("raw terminal seek");
    assert!(matches!(past, crate::SeekOutcome::PastEof { duration, .. }
        if duration == Duration::from_millis(1_669)));
    assert_eq!(source.playhead.position(), Duration::from_millis(1_669));
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    for (origin, expected_end) in [(576, 2_000), (0, 1_424)] {
        let mut exact = super::fixtures::extent(Some(Duration::from_secs(2)), origin, true);
        exact.frames = 2_000;
        exact.gapless = None;
        exact.media_origin = Some(Duration::from_millis(576));
        let mut owner = physical_source(exact).await;
        assert_eq!(owner.playhead.duration(), Some(Duration::from_secs(2)));
        assert_eq!(owner.media_end(), Some(Duration::from_millis(expected_end)));
        let pcm = drain(&mut owner, exact);
        assert_eq!(
            pcm.iter().map(|meta| u64::from(meta.frames)).sum::<u64>(),
            2_000
        );
        assert!(
            pcm.iter()
                .all(|meta| meta.media_end == Some(Duration::from_millis(expected_end)))
        );
        assert_eq!(owner.playhead.duration(), Some(Duration::from_secs(2)));
        assert_eq!(owner.media_end(), Some(Duration::from_millis(expected_end)));
    }
}

#[kithara::test(tokio)]
#[case::unknown(None)]
#[case::overestimated(Some(Duration::from_secs(60)))]
async fn first_nonzero_seek_keeps_full_duration_and_removes_padding_once(
    #[case] declared: Option<Duration>,
) {
    let extent = extent(declared, 0, true);
    let mut source = physical_source(extent).await;
    source
        .seek(Duration::from_millis(500))
        .expect("first nonzero seek");
    let chunks = drain(&mut source, extent);
    assert_eq!(
        chunks
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        469
    );
    assert_eq!(chunks.first().expect("sought head").frame_offset, 500);
    assert_eq!(chunks.last().expect("visible tail").frame_range().end, 969);
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    source
        .seek(Duration::from_millis(900))
        .expect("replay near EOF");
    let chunks = drain(&mut source, extent);
    assert_eq!(
        chunks
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        69
    );
    assert_eq!(chunks.first().expect("replayed head").frame_offset, 900);
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    source
        .seek(Duration::from_millis(960))
        .expect("seek inside physical visible tail");
    let tail = drain(&mut source, extent);
    assert_eq!(
        tail.iter().map(|meta| u64::from(meta.frames)).sum::<u64>(),
        9
    );
    assert_eq!(tail.first().expect("last visible head").frame_offset, 960);
    assert_eq!(
        tail.last().expect("last visible tail").frame_range().end,
        969
    );
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
}

#[kithara::test(tokio)]
#[case::unknown(None)]
#[case::overestimated(Some(Duration::from_secs(60)))]
async fn unknown_original_position_never_promotes_the_first_sought_suffix(
    #[case] declared: Option<Duration>,
) {
    let mut extent = extent(declared, 700, false);
    extent.media_origin = Some(Duration::from_millis(700));
    let mut source = physical_source(extent).await;
    let initial = source.playhead.duration();
    assert_eq!(source.media_end(), None);
    source
        .seek(Duration::from_millis(1_500))
        .expect("seek before original observation");
    assert_eq!(source.media_end(), None);
    let chunks = drain(&mut source, extent);
    assert_eq!(
        chunks
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        169
    );
    assert_eq!(chunks.first().expect("sought head").frame_offset, 1_500);
    assert_eq!(
        chunks.last().expect("visible tail").frame_range().end,
        1_669
    );
    assert_eq!(source.playhead.duration(), initial);
    assert!(
        chunks
            .iter()
            .all(|meta| meta.visible_origin.is_none() && meta.media_end.is_none())
    );
    assert_eq!(source.media_end(), Some(Duration::from_millis(969)));
    for mode in [
        GaplessMode::Disabled,
        GaplessMode::MediaOnly,
        GaplessMode::CodecPriming,
    ] {
        let mut media = super::fixtures::extent(None, 700, false);
        media.declared = Some(kithara_decode::DecoderDuration::MediaEnd(
            Duration::from_secs(1),
        ));
        media.media_origin = Some(Duration::from_millis(700));
        let mut owner = physical_source_with_mode(media, mode).await;
        let expected = if matches!(mode, GaplessMode::Disabled) {
            1_000
        } else {
            969
        };
        let endpoint = Some(Duration::from_millis(expected));
        assert_eq!(owner.playhead.duration(), None);
        assert_eq!(owner.media_end(), endpoint);
        owner
            .install_replacement(
                RecreateState {
                    media_info: Some(media_info(0)),
                    offset: 0,
                    cause: RecreateCause::HostRateChange,
                },
                Some(crate::pipeline::seek::ResumeTarget::Source(
                    crate::SourceEnd::new(1_500, NonZeroU32::new(1_000).expect("source rate")),
                )),
            )
            .expect("recreate before any original PCM");
        assert_eq!(owner.decode.active().visible_origin(), None);
        assert_eq!(owner.playhead.duration(), None);
        assert_eq!(owner.media_end(), endpoint);
        let sought = first_chunk(&mut owner);
        assert_eq!(sought.meta.visible_origin, None);
        assert_eq!(sought.meta.media_end, endpoint);
        assert_eq!(sought.meta.frame_offset, 1_500);
        assert_eq!(&sought.samples[..2], &[0.08; 2]);
        let tail = drain(&mut owner, media);
        assert_eq!(
            u64::from(sought.meta.frames)
                + tail.iter().map(|meta| u64::from(meta.frames)).sum::<u64>(),
            expected - 800
        );
        assert_eq!(owner.playhead.duration(), None);
        assert_eq!(owner.media_end(), endpoint);
    }
}

#[kithara::test(tokio)]
async fn exact_origin_survives_decoder_recreation_before_any_output() {
    let extent = extent(Some(Duration::from_secs(60)), 0, true);
    let mut source = physical_source(extent).await;
    source
        .install_replacement(
            RecreateState {
                media_info: Some(media_info(0)),
                offset: 0,
                cause: RecreateCause::HostRateChange,
            },
            Some(crate::pipeline::seek::ResumeTarget::Source(
                crate::SourceEnd::new(900, NonZeroU32::new(1_000).expect("source rate")),
            )),
        )
        .expect("recreate near EOF");
    let chunks = drain(&mut source, extent);
    assert_eq!(
        chunks
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        69
    );
    let first = chunks.first().expect("recreated head");
    assert_eq!(first.frame_offset, 900);
    assert_eq!(
        first
            .source_span
            .expect("original source mapping")
            .position_at(0),
        Some(Duration::from_millis(900))
    );
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    {
        let mut unrendered = physical_source(extent).await;
        assert_eq!(unrendered.resume.source_end(), None);
        unrendered.set_host_sample_rate(NonZeroU32::new(2_000).expect("initial host binding"));
        let first = first_chunk(&mut unrendered);
        assert_eq!(first.meta.spec.sample_rate.get(), 2_000);
        assert_eq!(first.meta.timestamp, Duration::from_millis(17));
        assert_eq!(first.meta.visible_origin, Some(Duration::from_millis(17)));
    }
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn actual_origin_survives_a_host_rate_recreation() {
    let extent = extent(None, 0, false);
    let mut source = physical_source(extent).await;
    let first = first_chunk(&mut source);
    assert_eq!(first.frames(), 239);
    assert_eq!(first.meta.timestamp, Duration::from_millis(17));
    assert_eq!(&first.samples[..2], &[0.0017; 2]);
    source.commit_source_end(
        crate::SourceEnd::new(256, first.spec().sample_rate),
        first.meta,
    );
    source.set_host_sample_rate(NonZeroU32::new(2_000).expect("new host rate"));
    let chunks = drain(&mut source, extent);
    assert_eq!(
        chunks
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        1_426
    );
    let first = chunks.first().expect("recreated head");
    assert_eq!(first.spec.sample_rate.get(), 2_000);
    assert_eq!(first.timestamp, Duration::from_millis(256));
    assert!(first.source_span.is_some());
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
    {
        let mut fractional = super::fixtures::extent(Some(Duration::from_secs(1)), 0, true);
        fractional.gapless = None;
        fractional.priming = Some(0);
        fractional.media_origin = Some(Duration::ZERO);
        let mut source = physical_source(fractional).await;
        let old_rate = NonZeroU32::new(48_000).expect("old host rate");
        let new_rate = NonZeroU32::new(44_100).expect("new host rate");
        source.set_host_sample_rate(old_rate);
        let old = first_chunk(&mut source);
        assert_eq!(old.spec().sample_rate, old_rate);
        source.commit_source_end(crate::SourceEnd::new(1, old_rate), old.meta);
        source.set_host_sample_rate(new_rate);
        let resumed = first_chunk(&mut source);
        let native = source.native_pcm_basis().expect("actual native landing");
        assert_eq!(native.frame_offset, 1);
        assert_eq!(
            native.timestamp,
            AudioSpec::new(2, new_rate)
                .duration_for(1)
                .expect("native frame one")
        );
        let span = resumed
            .meta
            .source_span
            .expect("retained source-clock relation");
        assert_eq!(span.sample_rate(), old_rate);
        assert_eq!(
            span.source_ratio_at(0),
            Some((1, std::num::NonZeroU128::MIN)),
            "the first retained PCM must start at the rendered SourceEnd despite native seek quantization"
        );
        assert_eq!(
            resumed.meta.timestamp,
            AudioSpec::new(2, old_rate)
                .duration_for(1)
                .expect("retained old frame one")
        );
    }
}

#[kithara::test(tokio)]
#[case::unknown(None)]
#[case::overestimated(Some(Duration::from_secs(60)))]
async fn a_short_source_refines_unknown_or_overestimated_length_after_tail_drain(
    #[case] declared: Option<Duration>,
) {
    let mut extent = extent(declared, 700, false);
    extent.frames = 64;
    let mut source = physical_source(extent).await;
    let initial = source.playhead.duration();
    assert_eq!(source.decode.active().visible_origin(), None);
    let chunks = drain(&mut source, extent);
    assert_eq!(chunks.len(), 1);
    assert_eq!(u64::from(chunks[0].frames), 16);
    assert_eq!(chunks[0].timestamp, Duration::from_millis(717));
    assert_eq!(chunks[0].frame_range().end, 733);
    assert_eq!(
        source.decode.active().visible_origin(),
        Some(Duration::from_millis(717))
    );
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(16)));
    assert_ne!(source.playhead.duration(), initial);
    assert_eq!(
        source
            .decode
            .active()
            .visible_origin()
            .and_then(|origin| origin.checked_add(source.playhead.duration()?)),
        Some(Duration::from_millis(733))
    );
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn first_nonzero_seek_after_original_pipeline_output_retains_its_origin() {
    let extent = extent(None, 700, false);
    let mut source = physical_source(extent).await;
    let original = first_chunk(&mut source);
    assert_eq!(original.frames(), 239);
    assert_eq!(original.meta.timestamp, Duration::from_millis(717));
    assert_eq!(&original.samples[..2], &[0.0017; 2]);
    assert_eq!(
        source.decode.active().visible_origin(),
        Some(Duration::from_millis(717))
    );
    assert_eq!(source.playhead.duration(), None);
    source
        .seek(Duration::from_millis(1_500))
        .expect("first nonzero raw seek");
    let chunks = drain(&mut source, extent);
    assert_eq!(
        chunks
            .iter()
            .map(|meta| u64::from(meta.frames))
            .sum::<u64>(),
        169
    );
    assert_eq!(
        chunks.first().expect("sought head").timestamp,
        Duration::from_millis(1_500)
    );
    assert_eq!(chunks.last().expect("sought tail").frame_range().end, 1_669);
    assert_eq!(
        source.decode.active().visible_origin(),
        Some(Duration::from_millis(717))
    );
    assert_eq!(source.playhead.duration(), Some(Duration::from_millis(952)));
}

#[kithara::test(tokio)]
#[case::cancelled(crate::TrackFailureKind::SourceCancelled)]
#[case::failed(crate::TrackFailureKind::Render)]
async fn failed_source_after_original_pcm_never_publishes_an_eof_length(
    #[case] failure: crate::TrackFailureKind,
) {
    let extent = extent(Some(Duration::from_secs(60)), 700, false);
    let mut source = physical_source(extent).await;
    let original = first_chunk(&mut source);
    assert_eq!(original.meta.timestamp, Duration::from_millis(717));
    assert_eq!(original.frames(), 239);
    let declared = source.playhead.duration();
    source.fail(failure, None);
    assert!(matches!(source.step_track(), TrackStep::Failed(found) if found == failure));
    assert_eq!(source.playhead.duration(), declared);
    assert_ne!(declared, Some(Duration::from_millis(239)));
    assert_eq!(
        source.decode.active().visible_origin(),
        Some(original.meta.timestamp)
    );
}

#[kithara::test(tokio)]
#[case::media_only(GaplessMode::MediaOnly)]
#[case::codec_priming(GaplessMode::CodecPriming)]
async fn a_raw_length_endpoint_follows_the_visible_origin_before_any_output(
    #[case] mode: GaplessMode,
) {
    let mut extent = extent(Some(Duration::from_secs(60)), 700, true);
    extent.media_origin = Some(Duration::from_millis(700));
    let source = physical_source_with_mode(extent, mode).await;
    assert_eq!(source.media_end(), Some(Duration::from_millis(59_969)));
}
