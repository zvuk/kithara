use kithara::{audio::DecoderBackend as EventDecoderBackend, decode::DecoderBackend};

use super::{cases::*, harness::*, imports::*, providers::*};

#[kithara::test(native, tokio, multi_thread, serial)]
#[case::synthetic(synthetic_sources().await, CUE)]
#[case::mp3_same(source_mp3_same().await, CUE)]
#[case::mp3_distinct(source_mp3_distinct().await, CUE)]
#[case::hls_plain(source_hls_same_plain().await, CUE)]
#[case::hls_drm(source_hls_same_drm().await, CUE)]
#[case::hls_mp3(source_hls_mp3_plain().await, CUE)]
#[case::hls_mp3_drm(source_hls_mp3_drm().await, CUE)]
#[case::ambient_trip_hop(source_ambient_trip_hop_provider().await, CUE)]
#[case::downtempo_house(source_downtempo_house_provider().await, CUE)]
#[case::techno_breakbeat(source_techno_breakbeat_provider().await, CUE)]
#[case::cross_style(source_cross_style_provider().await, CUE)]
#[case::tunnel(tunnel_sources().await, TUNNEL_CUE)]
#[case::newtechno(newtechno_sources().await, NEWTECHNO_PHRASE)]
#[case::library(prepared_sources(Provider::Library(crate::providers::LIBRARY)).await, CUE)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn provider_tempo_controls_measure_without_sync(
    #[case] prepared: PreparedSources,
    #[case] start: Start,
) {
    let mut failures = Vec::new();
    for (deck, source) in prepared.2.iter().enumerate() {
        if prepared.2[..deck].contains(source) {
            continue;
        }
        let ArtifactSource::Value(mut grid) = prepared.0.beat_grid(deck) else {
            panic!("control has a known fixture grid");
        };
        let native_bpm = grid.as_raw().bpm;
        #[cfg(not(target_os = "android"))]
        let mut reference = match prepared.0.control_reference(deck) {
            Some(asset) => Some(
                kithara_integration_tests::pcm_oracle::decode_native(asset.bytes())
                    .await
                    .expect("independent native control reference"),
            ),
            None => None,
        };
        #[cfg(target_os = "android")]
        let mut reference: Option<kithara_integration_tests::pcm_oracle::Audio> = None;
        let wrong_asset = prepared.0.wrong_control_reference(deck);
        #[cfg(not(target_os = "android"))]
        let mut wrong_source =
            kithara_integration_tests::pcm_oracle::decode_native(wrong_asset.bytes())
                .await
                .expect("independent wrong-source control reference");
        #[cfg(target_os = "android")]
        let mut wrong_source = kithara_integration_tests::pcm_oracle::Audio {
            samples: kithara_integration_tests::pcm_oracle::decode(wrong_asset.bytes())
                .await
                .expect("independent 48 kHz stereo wrong-source fixture"),
            channels: CHANNELS,
            sample_rate: 48_000,
        };
        for target in [native_bpm, 108.0, 120.0, 127.0, 132.0] {
            hang_tick!(format!(
                "{:?}: deck {deck}, native {native_bpm}, target {target}",
                prepared.0
            ));
            let (trace, captured) = super::control::capture(
                source.clone(),
                48_000,
                native_bpm,
                target,
                prepared.0.start_seconds(deck, start),
            )
            .await;
            let label = format!(
                "{:?} deck {deck} native {native_bpm} target {target}",
                prepared.0
            );
            let analysis_label = label.clone();
            let analysis_source = source.clone();
            let requested_start_seconds = prepared.0.start_seconds(deck, start);
            let has_score_markers = prepared.0.has_score_markers();
            let (captured, returned_reference, returned_wrong_source, returned_grid, measured) =
                kithara::platform::tokio::task::spawn_blocking(move || {
                    let label = analysis_label;
                    let source = analysis_source;
                    let pcm = captured.pcm.as_slice();
                    let measured = reference.as_ref().map_or_else(
                        || captured.observed_content_failures(&wrong_source, &grid),
                        |reference| captured.source_content_failures(reference, &wrong_source, &grid),
                    );
                    if !measured.is_empty() {
                        let diagnostic = serde_json::json!({
                            "label": label,
                            "source": source,
                            "requested_start_seconds": requested_start_seconds,
                            "failures": measured,
                            "flight_events": kithara_test_utils::flight::tail(),
                            "flight_probes": kithara_test_utils::flight::probes_tail(),
                        });
                        eprintln!("ORACLE_CONTROL_FAILURE {diagnostic}");
                    }
                    let supplemental = if has_score_markers {
                        marked_synchronization_failures(&label, &[pcm], CHANNELS, 48_000, target)
                    } else {
                        synchronization_failures(&label, &[pcm], CHANNELS, 48_000, target)
                    };
                    eprintln!("ORACLE_SUPPLEMENTAL_PEAKS {label}: {supplemental:?}");
                    let (markers, downbeats) =
                        kithara_integration_tests::cochlea::marked_rhythm_markers(pcm, CHANNELS, 48_000);
                    eprintln!(
                        "ORACLE_PCM source={source} start={} markers={markers:?} downbeats={downbeats:?}",
                        requested_start_seconds
                    );
                    (captured, reference, wrong_source, grid, measured)
                })
                .await
                .expect("provider control reference analysis must join");
            reference = returned_reference;
            wrong_source = returned_wrong_source;
            grid = returned_grid;
            let pcm = captured.pcm.as_slice();
            let artifact = format!(
                "{}-{deck}-{target}",
                kithara_integration_tests::audio_artifact::artifact_label()
            )
            .replace('.', "-");
            if let Some(mut tap) =
                kithara_integration_tests::audio_artifact::AudioArtifactTap::from_env(
                    &artifact, 48_000, CHANNELS,
                )
                .expect("control artifact")
            {
                tap.push(pcm);
                tap.evidence("control", captured.evidence());
                tap.evidence("grid", serde_json::json!(grid.as_raw()));
                tap.evidence(
                    "source",
                    serde_json::json!({
                        "path": source,
                        "start": prepared.0.start_seconds(deck, start),
                        "native_bpm": native_bpm,
                        "target_bpm": target,
                    }),
                );
            }
            drop(captured);
            drop(trace);
            eprintln!("ORACLE_CONTROL {label}: {measured:?}");
            failures.extend(measured);
            hang_reset!();
        }
    }
    assert!(
        failures.is_empty(),
        "provider tempo controls failed: {failures:?}"
    );
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn run(case: SyncCase, prepared: PreparedSources, start: Start) {
    run_with_decoder(case, prepared, start, None).await;
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn run_with_decoder(
    case: SyncCase,
    prepared: PreparedSources,
    start: Start,
    decoder: Option<(DecoderBackend, EventDecoderBackend)>,
) {
    let provider = prepared.0;
    let expected_frames: usize = (f64::from(case.sample_rate) * 60.0 / case.ride.final_bpm() * 6.0)
        .round()
        .as_();
    let expected_samples = expected_frames * usize::from(CHANNELS);
    let mut tracks = Vec::with_capacity(case.decks);
    let mut request_failures = Vec::new();
    for audible_deck in 0..case.decks {
        let wrong_source = kithara_integration_tests::pcm_oracle::decode(
            provider.wrong_control_reference(audible_deck).bytes(),
        )
        .await
        .expect("independent wrong-source payload");
        let wrong_source = kithara_integration_tests::pcm_oracle::Audio {
            samples: wrong_source,
            channels: CHANNELS,
            sample_rate: 48_000,
        };
        hang_tick!(format!(
            "{} {provider:?}: capturing deck {audible_deck}",
            case.id
        ));
        let mut harness = if let Some((backend, expected)) = decoder {
            let harness = ProductHarness::new_with_decoder(
                case,
                &prepared,
                start,
                Audible::Deck(audible_deck),
                backend,
            )
            .await;
            harness.assert_decoder_backend(expected);
            harness
        } else {
            ProductHarness::new(case, &prepared, start, Audible::Deck(audible_deck)).await
        };
        harness.run_operations(case).await;
        harness.ride_tempo(case).await;
        let pcm = harness.capture(case).await;
        assert_eq!(
            pcm.len(),
            expected_samples,
            "{} {provider:?}: deck {audible_deck} capture must contain six complete beats",
            case.id,
        );
        request_failures.extend(
            harness
                .physical_failures(case, audible_deck, &pcm, &wrong_source, None)
                .await,
        );
        request_failures.extend(harness.grid_binding_failures(case).await);
        request_failures.extend(harness.grid_anchor_failures(case, None).await);
        tracks.push(pcm);
        request_failures.append(&mut harness.failures);
        drop(harness);
        hang_reset!();
    }
    let track_slices = tracks.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let label = format!("{} {provider:?}", case.id);
    let supplemental = if provider.has_score_markers() {
        marked_synchronization_failures(
            &label,
            &track_slices,
            CHANNELS,
            case.sample_rate,
            case.ride.final_bpm(),
        )
    } else {
        synchronization_failures(
            &label,
            &track_slices,
            CHANNELS,
            case.sample_rate,
            case.ride.final_bpm(),
        )
    };
    eprintln!("ORACLE_SUPPLEMENTAL_PEAKS {label}: {supplemental:?}");
    let failures = request_failures;
    assert!(
        failures.is_empty(),
        "product synchronization assertion failed for {} {provider:?}:\n{}",
        case.id,
        failures.join("\n"),
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::mp3(source_mp3_same().await)]
#[case::drm(source_hls_same_drm().await)]
async fn encoded_rhythmic_controls_reach_the_pcm_oracle(#[case] prepared: PreparedSources) {
    let provider = prepared.0;
    let mut harness = ProductHarness::new(ONE_DECK, &prepared, CUE, Audible::Deck(0)).await;
    let pcm = harness.capture(ONE_DECK).await;
    let mut failures = synchronization_failures(
        &format!("encoded rhythmic control {provider:?}"),
        &[pcm.as_slice()],
        CHANNELS,
        ONE_DECK.sample_rate,
        START_BPM,
    );
    failures.append(&mut harness.failures);
    assert!(
        failures.is_empty(),
        "encoded rhythmic control {provider:?} failed:\n{}",
        failures.join("\n"),
    );
    drop(harness);
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(600)))]
#[case::play_sync_seek(PLAY_SYNC_SEEK, source_synthetic().await)]
#[case::play_seek_sync(PLAY_SEEK_SYNC, source_synthetic().await)]
#[case::seek_play_sync(SEEK_PLAY_SYNC, source_synthetic().await)]
#[case::seek_sync_play(SEEK_SYNC_PLAY, source_synthetic().await)]
#[case::sync_play_seek(SYNC_PLAY_SEEK, source_synthetic().await)]
#[case::sync_seek_play(SYNC_SEEK_PLAY, source_synthetic().await)]
#[case::sequential_sync(SEQUENTIAL_SYNC, source_synthetic().await)]
#[case::paused_sync_then_play(PAUSED_SYNC, source_synthetic().await)]
#[case::four_deck_sequential_sync(FOUR_DECK_SYNC, source_synthetic().await)]
#[case::tempo_up_120hz(TEMPO_UP_120, source_synthetic().await)]
#[case::tempo_down_30hz(TEMPO_DOWN_30, source_synthetic().await)]
#[case::ambient_trip_hop(AMBIENT_TRIP_HOP_SYNC, source_ambient_trip_hop_provider().await)]
#[case::downtempo_house(DOWNTEMPO_HOUSE_SYNC, source_downtempo_house_provider().await)]
#[case::techno_breakbeat(TECHNO_BREAKBEAT_SYNC, source_techno_breakbeat_provider().await)]
#[case::cross_style_four_deck(CROSS_STYLE_SYNC, source_cross_style_provider().await)]
async fn wav_product_rows_reach_the_pcm_oracle(
    #[case] case: SyncCase,
    #[case] provider: PreparedSources,
) {
    run(case, provider, CUE).await;
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(600)))]
#[case::hls_same_play_sync_seek(source_hls_same_plain().await, PLAY_SYNC_SEEK)]
#[case::hls_same_play_seek_sync(source_hls_same_plain().await, PLAY_SEEK_SYNC)]
#[case::hls_same_seek_play_sync(source_hls_same_plain().await, SEEK_PLAY_SYNC)]
#[case::hls_same_seek_sync_play(source_hls_same_plain().await, SEEK_SYNC_PLAY)]
#[case::hls_same_sync_play_seek(source_hls_same_plain().await, SYNC_PLAY_SEEK)]
#[case::hls_same_sync_seek_play(source_hls_same_plain().await, SYNC_SEEK_PLAY)]
#[case::hls_same_sequential_sync(source_hls_same_plain().await, SEQUENTIAL_SYNC)]
#[case::hls_same_paused_sync_then_play(source_hls_same_plain().await, PAUSED_SYNC)]
#[case::hls_same_four_deck_sequential_sync(source_hls_same_plain().await, FOUR_DECK_SYNC)]
#[case::hls_same_tempo_up_120hz(source_hls_same_plain().await, TEMPO_UP_120)]
#[case::hls_same_tempo_down_30hz(source_hls_same_plain().await, TEMPO_DOWN_30)]
#[case::drm_same_play_sync_seek(source_hls_same_drm().await, PLAY_SYNC_SEEK)]
#[case::drm_same_play_seek_sync(source_hls_same_drm().await, PLAY_SEEK_SYNC)]
#[case::drm_same_seek_play_sync(source_hls_same_drm().await, SEEK_PLAY_SYNC)]
#[case::drm_same_seek_sync_play(source_hls_same_drm().await, SEEK_SYNC_PLAY)]
#[case::drm_same_sync_play_seek(source_hls_same_drm().await, SYNC_PLAY_SEEK)]
#[case::drm_same_sync_seek_play(source_hls_same_drm().await, SYNC_SEEK_PLAY)]
#[case::drm_same_sequential_sync(source_hls_same_drm().await, SEQUENTIAL_SYNC)]
#[case::drm_same_paused_sync_then_play(source_hls_same_drm().await, PAUSED_SYNC)]
#[case::drm_same_four_deck_sequential_sync(source_hls_same_drm().await, FOUR_DECK_SYNC)]
#[case::drm_same_tempo_up_120hz(source_hls_same_drm().await, TEMPO_UP_120)]
#[case::drm_same_tempo_down_30hz(source_hls_same_drm().await, TEMPO_DOWN_30)]
#[case::mp3_same_play_sync_seek(source_mp3_same().await, PLAY_SYNC_SEEK)]
#[case::mp3_same_play_seek_sync(source_mp3_same().await, PLAY_SEEK_SYNC)]
#[case::mp3_same_seek_play_sync(source_mp3_same().await, SEEK_PLAY_SYNC)]
#[case::mp3_same_seek_sync_play(source_mp3_same().await, SEEK_SYNC_PLAY)]
#[case::mp3_same_sync_play_seek(source_mp3_same().await, SYNC_PLAY_SEEK)]
#[case::mp3_same_sync_seek_play(source_mp3_same().await, SYNC_SEEK_PLAY)]
#[case::mp3_same_sequential_sync(source_mp3_same().await, SEQUENTIAL_SYNC)]
#[case::mp3_same_paused_sync_then_play(source_mp3_same().await, PAUSED_SYNC)]
#[case::mp3_same_four_deck_sequential_sync(source_mp3_same().await, FOUR_DECK_SYNC)]
#[case::mp3_same_tempo_up_120hz(source_mp3_same().await, TEMPO_UP_120)]
#[case::mp3_same_tempo_down_30hz(source_mp3_same().await, TEMPO_DOWN_30)]
#[case::mp3_distinct_play_sync_seek(source_mp3_distinct().await, PLAY_SYNC_SEEK)]
#[case::mp3_distinct_play_seek_sync(source_mp3_distinct().await, PLAY_SEEK_SYNC)]
#[case::mp3_distinct_seek_play_sync(source_mp3_distinct().await, SEEK_PLAY_SYNC)]
#[case::mp3_distinct_seek_sync_play(source_mp3_distinct().await, SEEK_SYNC_PLAY)]
#[case::mp3_distinct_sync_play_seek(source_mp3_distinct().await, SYNC_PLAY_SEEK)]
#[case::mp3_distinct_sync_seek_play(source_mp3_distinct().await, SYNC_SEEK_PLAY)]
#[case::mp3_distinct_sequential_sync(source_mp3_distinct().await, SEQUENTIAL_SYNC)]
#[case::mp3_distinct_paused_sync_then_play(source_mp3_distinct().await, PAUSED_SYNC)]
#[case::mp3_distinct_four_deck_sequential_sync(source_mp3_distinct().await, FOUR_DECK_SYNC)]
#[case::mp3_distinct_tempo_up_120hz(source_mp3_distinct().await, TEMPO_UP_120)]
#[case::mp3_distinct_tempo_down_30hz(source_mp3_distinct().await, TEMPO_DOWN_30)]
#[case::hls_mp3_play_sync_seek(source_hls_mp3_plain().await, PLAY_SYNC_SEEK)]
#[case::hls_mp3_play_seek_sync(source_hls_mp3_plain().await, PLAY_SEEK_SYNC)]
#[case::hls_mp3_seek_play_sync(source_hls_mp3_plain().await, SEEK_PLAY_SYNC)]
#[case::hls_mp3_seek_sync_play(source_hls_mp3_plain().await, SEEK_SYNC_PLAY)]
#[case::hls_mp3_sync_play_seek(source_hls_mp3_plain().await, SYNC_PLAY_SEEK)]
#[case::hls_mp3_sync_seek_play(source_hls_mp3_plain().await, SYNC_SEEK_PLAY)]
#[case::hls_mp3_sequential_sync(source_hls_mp3_plain().await, SEQUENTIAL_SYNC)]
#[case::hls_mp3_paused_sync_then_play(source_hls_mp3_plain().await, PAUSED_SYNC)]
#[case::hls_mp3_four_deck_sequential_sync(source_hls_mp3_plain().await, FOUR_DECK_SYNC)]
#[case::hls_mp3_tempo_up_120hz(source_hls_mp3_plain().await, TEMPO_UP_120)]
#[case::hls_mp3_tempo_down_30hz(source_hls_mp3_plain().await, TEMPO_DOWN_30)]
#[case::drm_mp3_play_sync_seek(source_hls_mp3_drm().await, PLAY_SYNC_SEEK)]
#[case::drm_mp3_play_seek_sync(source_hls_mp3_drm().await, PLAY_SEEK_SYNC)]
#[case::drm_mp3_seek_play_sync(source_hls_mp3_drm().await, SEEK_PLAY_SYNC)]
#[case::drm_mp3_seek_sync_play(source_hls_mp3_drm().await, SEEK_SYNC_PLAY)]
#[case::drm_mp3_sync_play_seek(source_hls_mp3_drm().await, SYNC_PLAY_SEEK)]
#[case::drm_mp3_sync_seek_play(source_hls_mp3_drm().await, SYNC_SEEK_PLAY)]
#[case::drm_mp3_sequential_sync(source_hls_mp3_drm().await, SEQUENTIAL_SYNC)]
#[case::drm_mp3_paused_sync_then_play(source_hls_mp3_drm().await, PAUSED_SYNC)]
#[case::drm_mp3_four_deck_sequential_sync(source_hls_mp3_drm().await, FOUR_DECK_SYNC)]
#[case::drm_mp3_tempo_up_120hz(source_hls_mp3_drm().await, TEMPO_UP_120)]
#[case::drm_mp3_tempo_down_30hz(source_hls_mp3_drm().await, TEMPO_DOWN_30)]
async fn real_media_product_rows_reach_the_pcm_oracle(
    #[case] provider: PreparedSources,
    #[case] case: SyncCase,
) {
    run(case, provider, CUE).await;
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(600)))]
#[case::tunnel_play_sync_seek(tunnel_sources().await, TUNNEL_CUE, PLAY_SYNC_SEEK.gridded())]
#[case::tunnel_play_seek_sync(tunnel_sources().await, TUNNEL_CUE, PLAY_SEEK_SYNC.gridded())]
#[case::tunnel_seek_play_sync(tunnel_sources().await, TUNNEL_CUE, SEEK_PLAY_SYNC.gridded())]
#[case::tunnel_seek_sync_play(tunnel_sources().await, TUNNEL_CUE, SEEK_SYNC_PLAY.gridded())]
#[case::tunnel_sync_play_seek(tunnel_sources().await, TUNNEL_CUE, SYNC_PLAY_SEEK.gridded())]
#[case::tunnel_sync_seek_play(tunnel_sources().await, TUNNEL_CUE, SYNC_SEEK_PLAY.gridded())]
#[case::tunnel_sequential_sync(tunnel_sources().await, TUNNEL_CUE, SEQUENTIAL_SYNC.gridded())]
#[case::tunnel_paused_sync_then_play(tunnel_sources().await, TUNNEL_CUE, PAUSED_SYNC.gridded())]
#[case::tunnel_four_deck_sequential_sync(tunnel_sources().await, TUNNEL_CUE, FOUR_DECK_SYNC.gridded())]
#[case::tunnel_tempo_up_120hz(tunnel_sources().await, TUNNEL_CUE, TEMPO_UP_120.gridded())]
#[case::tunnel_tempo_down_30hz(tunnel_sources().await, TUNNEL_CUE, TEMPO_DOWN_30.gridded())]
#[case::newtechno_play_sync_seek(newtechno_sources().await, NEWTECHNO_PHRASE, PLAY_SYNC_SEEK.gridded())]
#[case::newtechno_play_seek_sync(newtechno_sources().await, NEWTECHNO_PHRASE, PLAY_SEEK_SYNC.gridded())]
#[case::newtechno_seek_play_sync(newtechno_sources().await, NEWTECHNO_PHRASE, SEEK_PLAY_SYNC.gridded())]
#[case::newtechno_seek_sync_play(newtechno_sources().await, NEWTECHNO_PHRASE, SEEK_SYNC_PLAY.gridded())]
#[case::newtechno_sync_play_seek(newtechno_sources().await, NEWTECHNO_PHRASE, SYNC_PLAY_SEEK.gridded())]
#[case::newtechno_sync_seek_play(newtechno_sources().await, NEWTECHNO_PHRASE, SYNC_SEEK_PLAY.gridded())]
#[case::newtechno_sequential_sync(newtechno_sources().await, NEWTECHNO_PHRASE, SEQUENTIAL_SYNC.gridded())]
#[case::newtechno_paused_sync_then_play(newtechno_sources().await, NEWTECHNO_PHRASE, PAUSED_SYNC.gridded())]
#[case::newtechno_four_deck_sequential_sync(newtechno_sources().await, NEWTECHNO_PHRASE, FOUR_DECK_SYNC.gridded())]
#[case::newtechno_tempo_up_120hz(newtechno_sources().await, NEWTECHNO_PHRASE, TEMPO_UP_120.gridded())]
#[case::newtechno_tempo_down_30hz(newtechno_sources().await, NEWTECHNO_PHRASE, TEMPO_DOWN_30.gridded())]
async fn real_track_product_rows_reach_the_pcm_oracle(
    #[case] provider: PreparedSources,
    #[case] start: Start,
    #[case] case: SyncCase,
) {
    run(case, provider, start).await;
}

#[kithara::fixture]
pub(crate) async fn synthetic_sources() -> PreparedSources {
    prepared_sources(Provider::Synthetic).await
}
#[kithara::fixture]
pub(crate) async fn sweep_sources() -> PreparedSources {
    prepared_sources(Provider::Sweep).await
}
#[kithara::fixture]
pub(crate) async fn mixed_sources() -> PreparedSources {
    prepared_sources(Provider::HlsMp3(HlsProtection::Plain)).await
}

#[kithara::fixture]
async fn source_mp3_same() -> PreparedSources {
    prepared_sources(Provider::Mp3Same).await
}

#[kithara::fixture]
async fn source_hls_same_drm() -> PreparedSources {
    prepared_sources(Provider::HlsSame(HlsProtection::Drm)).await
}

#[kithara::fixture]
async fn source_synthetic() -> PreparedSources {
    prepared_sources(Provider::Synthetic).await
}

#[kithara::fixture]
async fn source_ambient_trip_hop_provider() -> PreparedSources {
    prepared_sources(AMBIENT_TRIP_HOP_PROVIDER).await
}

#[kithara::fixture]
async fn source_downtempo_house_provider() -> PreparedSources {
    prepared_sources(DOWNTEMPO_HOUSE_PROVIDER).await
}

#[kithara::fixture]
async fn source_techno_breakbeat_provider() -> PreparedSources {
    prepared_sources(TECHNO_BREAKBEAT_PROVIDER).await
}

#[kithara::fixture]
async fn source_cross_style_provider() -> PreparedSources {
    prepared_sources(CROSS_STYLE_PROVIDER).await
}

#[kithara::fixture]
async fn source_hls_same_plain() -> PreparedSources {
    prepared_sources(Provider::HlsSame(HlsProtection::Plain)).await
}

#[kithara::fixture]
async fn source_mp3_distinct() -> PreparedSources {
    prepared_sources(Provider::Mp3Distinct).await
}

#[kithara::fixture]
async fn source_hls_mp3_plain() -> PreparedSources {
    prepared_sources(Provider::HlsMp3(HlsProtection::Plain)).await
}

#[kithara::fixture]
async fn source_hls_mp3_drm() -> PreparedSources {
    prepared_sources(Provider::HlsMp3(HlsProtection::Drm)).await
}

#[kithara::fixture]
pub(crate) async fn tunnel_sources() -> PreparedSources {
    prepared_sources(Provider::Library(TUNNEL)).await
}

#[kithara::fixture]
pub(crate) async fn newtechno_sources() -> PreparedSources {
    prepared_sources(Provider::Library(NEWTECHNO)).await
}

#[cfg(not(target_os = "android"))]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    timeout(Duration::from_secs(60)),
    tracing("info")
)]
#[case::paused_sync_then_play(PAUSED_SYNC)]
#[case::play_sync_seek(PLAY_SYNC_SEEK)]
async fn mixed_mp3_and_hls_keep_the_pcm_contract_on_symphonia(#[case] case: SyncCase) {
    let prepared = source_hls_mp3_plain().await;
    run_with_decoder(
        case,
        prepared,
        CUE,
        Some((DecoderBackend::Symphonia, EventDecoderBackend::Symphonia)),
    )
    .await;
}
