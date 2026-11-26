use kithara::{
    abr::{AbrEvent, AbrReason},
    hls::AbrMode,
    platform::sync::Arc,
};
use kithara_integration_tests::{
    HlsFixtureBuilder, auto, cochlea::synchronization_failures, fixture_protocol::DelayRule,
    grid::Start, kithara, pcm_oracle,
};
use kithara_test_fixtures::{assets::rhythm_wav_deck_a_120bpm_48k, signal};

use super::{
    Audible, CHANNELS, PreparedSources, ProductHarness, SEQUENTIAL_SYNC, START_BPM,
    synthetic_sources,
};

async fn rhythmic_ladder(mode: AbrMode) -> PreparedSources {
    let mut sources = synthetic_sources().await;
    sources.real_network = true;
    let sample_rate = SEQUENTIAL_SYNC.sample_rate;
    let segment_frames = usize::try_from(sample_rate).expect("fixture sample rate");
    let segment_bytes = segment_frames * usize::from(CHANNELS) * size_of::<i16>();
    let wav = rhythm_wav_deck_a_120bpm_48k();
    let header_bytes = signal::header(sample_rate, CHANNELS, None).len();
    let (init, pcm) = wav.bytes().split_at(header_bytes);
    assert!(pcm.len().is_multiple_of(segment_bytes));
    let init = Arc::new(init.to_vec());
    let mut ladder = HlsFixtureBuilder::new()
        .variant_count(2)
        .segments_per_variant(pcm.len() / segment_bytes)
        .segment_size(segment_bytes)
        .segment_duration_secs(1.0)
        .custom_data(Arc::new(pcm.to_vec()))
        .init_data_per_variant(vec![init; 2])
        .variant_bandwidths(vec![5_000_000, 1_000_000])
        .codecs("wav".to_owned());
    if matches!(mode, AbrMode::Auto(_)) {
        ladder = ladder.delay_rules(vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(3),
            delay_ms: 500,
            ..Default::default()
        }]);
    }
    // Register delayed responses on the same real clock as ABR's network samples.
    let hls = kithara::platform::flash::with_ambient(false, sources.server.create_hls(ladder))
        .await
        .expect("local HLS ladder for the authored rhythm");
    sources.paths[0] = hls.master_url().to_string();
    sources
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    timeout(Long),
    tracing("kithara_abr=debug,kithara_hls=debug")
)]
#[case::manual_then_auto(AbrMode::manual(1))]
#[case::autonomous(auto(0))]
async fn a_synced_hls_rung_change_keeps_the_pcm_beat_grid(#[case] mode: AbrMode) {
    let sources = rhythmic_ladder(mode).await;
    let case = SEQUENTIAL_SYNC.gridded();
    let second = usize::try_from(case.sample_rate).expect("fixture sample rate");
    let mut tracks = Vec::new();
    for switch in [false, true] {
        let mut rig =
            ProductHarness::new(case, &sources, Start::Seconds(0.0), Audible::Deck(0)).await;
        rig.request_sync(case).await;
        rig.settle(case, 32).await;
        let mut before = rig.capture_source_frames(case, second * 2, 128).await;
        let deck = &rig.decks[0];
        let handle = deck.current_abr_handle().expect("sounding HLS track");
        let mut events = deck.subscribe::<AbrEvent>();
        assert_eq!(handle.current_variant_index(), Some(0));
        if switch {
            handle.set_mode(mode).expect("change the quality mode");
        }
        let after = rig.capture_source_frames(case, second * 5, 128).await;
        assert_eq!(handle.current_variant_index(), Some(usize::from(switch)));
        let applied = std::iter::from_fn(|| events.try_recv().ok()).find_map(|received| {
            match received.event {
                AbrEvent::VariantApplied { from, to, reason }
                    if from.get() == 0 && to.get() == 1 =>
                {
                    Some(reason)
                }
                _ => None,
            }
        });
        match (switch, mode) {
            (false, _) => assert_eq!(applied, None),
            (true, AbrMode::Manual(_)) => assert_eq!(applied, Some(AbrReason::ManualOverride)),
            (true, AbrMode::Auto(_)) => assert!(
                matches!(
                    applied,
                    Some(
                        AbrReason::DownSwitch
                            | AbrReason::UrgentDownSwitch
                            | AbrReason::EscapeStalled
                    )
                ),
                "expected a managed downswitch, got {applied:?}"
            ),
        }
        let wrong = pcm_oracle::prepared(&sources.provider().wrong_control_reference(0))
            .expect("independent wrong-source control");
        let failures = rig.physical_failures(case, 0, &after, &wrong, None).await;
        assert!(failures.is_empty(), "switch={switch}: {failures:#?}");
        assert!(
            rig.failures.is_empty(),
            "switch={switch}: {:?}",
            rig.failures
        );
        before.extend(after);
        if matches!(mode, AbrMode::Manual(_)) {
            if switch {
                handle
                    .set_mode(auto(1))
                    .expect("return to automatic quality");
                assert!(matches!(handle.mode(), Some(AbrMode::Auto(_))));
            }
            before.extend(rig.capture_source_frames(case, second * 2, 128).await);
        }
        drop(rig);
        tracks.push(before);
    }
    let stems: Vec<_> = tracks.iter().map(Vec::as_slice).collect();
    let failures = synchronization_failures(
        "fixed quality versus HLS handoff",
        &stems,
        CHANNELS,
        case.sample_rate,
        START_BPM,
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Long))]
#[case::after_two_seconds(2)]
#[case::after_four_seconds(4)]
async fn a_synced_aac_to_flac_promotion_keeps_native_pcm_time(#[case] switch_at: usize) {
    use kithara::{link::SyncStatus, stream::AudioCodec};
    use kithara_integration_tests::phase_continuity::{
        FREQ_HZ, READ_FRAMES_AFTER_SEEK, SAMPLE_RATE, SinePhaseSpec, check_against_previous,
    };
    use num_traits::ToPrimitive;

    let mut sources = synthetic_sources().await;
    let hls = sources
        .server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(3)
                .segments_per_variant(30)
                .segment_duration_secs(2.0)
                .packaged_audio_sine_aac_lc(SAMPLE_RATE, CHANNELS, FREQ_HZ)
                .override_variant_codec(2, AudioCodec::Flac),
        )
        .await
        .expect("registered mixed-codec sine ladder");
    sources.paths[0] = hls.master_url().to_string();

    let case = super::SyncCase {
        sample_rate: SAMPLE_RATE,
        ..super::ONE_DECK
    }
    .gridded();
    let second = usize::try_from(case.sample_rate).expect("fixture sample rate");
    let channels = usize::from(CHANNELS);
    let mut captures = Vec::new();
    let mut positions = Vec::new();
    for switch in [false, true] {
        let mut rig =
            ProductHarness::new(case, &sources, Start::Seconds(0.0), Audible::Deck(0)).await;
        rig.request_sync(case).await;
        rig.settle(case, 32).await;
        let mut pcm = rig.capture_frames(case, switch_at * second, 128).await;
        let deck = &rig.decks[0];
        let handle = deck.current_abr_handle().expect("sounding HLS track");
        let mut events = deck.subscribe::<AbrEvent>();
        assert_eq!(handle.current_variant_index(), Some(0));
        if switch {
            handle
                .set_mode(AbrMode::manual(2))
                .expect("select the FLAC rung");
        }
        pcm.extend(
            rig.capture_frames(case, (8 - switch_at) * second, 128)
                .await,
        );

        let final_state = rig.decks[0].read_snapshot().await;
        assert_eq!(final_state.len(), 1);
        let (track, slot) = &final_state[0];
        let applied = std::iter::from_fn(|| events.try_recv().ok()).any(|received| {
            matches!(received.event, AbrEvent::VariantApplied { from, to, reason: AbrReason::ManualOverride }
                if from.get() == 0 && to.get() == 2)
        });
        let expected = if switch { 2 } else { 0 };
        assert_eq!(applied, switch);
        assert_eq!(handle.current_variant_index(), Some(expected));
        assert!(matches!(
            track.sync,
            SyncStatus::On | SyncStatus::Correcting { .. }
        ));
        let source = slot.source.expect("consumed source");
        assert_eq!(source.variant_index, Some(expected));
        positions.push(
            source
                .spec
                .frame_at(track.track.position)
                .expect("logical playhead frame"),
        );
        if switch {
            let mut decoded = rig.decoded[0].lock();
            let chunks = &decoded.view().chunks;
            let (outgoing, tail) = chunks
                .values()
                .rfind(|(meta, _)| meta.variant_index == Some(0))
                .expect("decoded outgoing AAC");
            let (incoming, head) = chunks
                .values()
                .find(|(meta, _)| meta.variant_index == Some(2))
                .expect("decoded incoming FLAC");
            let frames = READ_FRAMES_AFTER_SEEK;
            let samples = frames * channels;
            let mut anchor = None;
            let reference = check_against_previous(
                &mut anchor,
                0.0,
                &tail[tail.len() - samples..],
                channels,
                SinePhaseSpec::default_440(),
                "decoded outgoing PCM",
            );
            assert!(
                reference.is_none() && anchor.is_some(),
                "decoded outgoing PCM measures the reference phase: {outgoing:?}"
            );
            let seconds = frames.to_f64().expect("bounded phase window") / f64::from(SAMPLE_RATE);
            let native_drift = check_against_previous(
                &mut anchor,
                seconds,
                &head[..samples],
                channels,
                SinePhaseSpec::default_440(),
                "decoded incoming PCM",
            );
            assert!(
                native_drift.is_none() && anchor.is_some_and(|(at, _)| at == seconds),
                "decoders meet at one phase before Warp: outgoing={outgoing:?} \
                 incoming={incoming:?} drift={native_drift:?}"
            );
            drop(decoded);
        }
        drop(rig);
        captures.push(pcm);
    }
    let [fixed, switched] = captures.as_slice() else {
        panic!("paired fixed and switched recordings");
    };
    let edit = fixed
        .chunks_exact(channels)
        .zip(switched.chunks_exact(channels))
        .position(|(left, right)| left != right)
        .expect("the codec change reaches the recorded PCM");
    let frame = edit
        .checked_sub(READ_FRAMES_AFTER_SEEK)
        .expect("enough PCM before the cut");
    let cut = edit.to_f64().expect("capture frame") / f64::from(SAMPLE_RATE);
    assert!(
        frame > second * switch_at,
        "PCM must agree before the quality command"
    );
    let mut failures = Vec::new();
    for (label, pcm) in [("fixed AAC", fixed), ("AAC to FLAC", switched)] {
        let mut anchor = None;
        assert!(
            check_against_previous(
                &mut anchor,
                cut - READ_FRAMES_AFTER_SEEK.to_f64().expect("window width")
                    / f64::from(SAMPLE_RATE),
                &pcm[frame * channels..edit * channels],
                channels,
                SinePhaseSpec::default_440(),
                label,
            )
            .is_none()
        );
        assert!(anchor.is_some(), "audible PCM before cut");
        let drift = check_against_previous(
            &mut anchor,
            cut,
            &pcm[edit * channels..(edit + READ_FRAMES_AFTER_SEEK) * channels],
            channels,
            SinePhaseSpec::default_440(),
            label,
        );
        assert_eq!(anchor.expect("audible PCM after cut").0, cut);
        failures.extend(drift);
    }
    assert!(
        failures.is_empty() && positions[0] == positions[1],
        "capture cut={edit}, phase={failures:?}, logical playheads={positions:?}"
    );
}
