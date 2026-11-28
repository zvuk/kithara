use kithara::{
    link::{GridAnswer, LinkedHostCommand, SyncStatus},
    platform::sync::Arc,
    play::{ArtifactSource, ResourceConfig, ResourceSrc},
    queue::QueueEvent,
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior,
    cochlea::{marked_rhythm_markers, synchronization_failures},
    grid::Start,
    kithara, memory_asset_store,
};
use kithara_test_fixtures::assets::rhythm_wav_deck_a_120bpm_48k;

use super::{
    Audible, CHANNELS, HlsProtection, ONE_DECK, ONE_DECK_RESAMPLED, ProductHarness, Provider,
    START_BPM, SyncCase, prepared_sources,
};

/// The Host tempo around the replacement.
enum HostTempo {
    /// The Host holds the track's own tempo.
    Track,
    /// The Host holds another tempo before SYNC is pressed between its blocks.
    Held(f64),
    /// The Host slews to another tempo while the replacement loads.
    Ramp(f64),
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Long))]
#[case::constant_sync(ONE_DECK, HostTempo::Track)]
#[case::off_the_track_tempo_resampled(ONE_DECK_RESAMPLED, HostTempo::Held(126.0))]
#[case::during_a_tempo_ramp(ONE_DECK, HostTempo::Ramp(124.0))]
async fn same_item_catalogue_replacement_keeps_the_linked_pcm_phase(
    #[case] case: SyncCase,
    #[case] tempo: HostTempo,
) {
    let second = usize::try_from(case.sample_rate).expect("fixture sample rate");
    let sources = prepared_sources(Provider::HlsSame(HlsProtection::Plain)).await;
    let mut captures = Vec::new();
    let mut positions = Vec::new();
    for replace in [false, true] {
        let mut rig =
            ProductHarness::new(case, &sources, Start::Seconds(0.0), Audible::Deck(0)).await;
        if let HostTempo::Held(bpm) = tempo {
            rig.set_tempo(case, bpm, true).await;
            // Once the Host holds the tempo the deck enters it at one constant
            // rate; off the block grid, the replacement resumes that course at
            // a frame whose rounded advance a fresh course would not reach.
            rig.settle(case, 32).await;
            let _ = rig.render(case, 37).await;
        }
        rig.request_sync(case).await;
        rig.settle(case, 32).await;
        let mut pcm = rig.capture_frames(case, 2 * second, 128).await;

        let control = rig.decks[0].control().clone();
        let item = control.current().expect("sounding track").id;
        let mut events = control.subscribe::<QueueEvent>();
        let requested = control.position_seconds().expect("outgoing playhead");
        let original = control.current().expect("original source").url;
        let wav = rhythm_wav_deck_a_120bpm_48k();
        let gate = sources.server.register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(wav.bytes().to_vec()),
                content_type: Some(wav.entry().content_type),
            },
            delivery: Delivery::Gated { after_bytes: 0 },
        });
        let replacement = gate.child_url("equivalent.wav");
        if replace {
            let incoming = ResourceConfig::for_src(
                ResourceSrc::parse(replacement.as_str()).expect("local WAV replacement"),
            )
            .downloader(sources.server.modeled_downloader())
            .store(memory_asset_store())
            .build();
            let owner = control.clone();
            rig.host
                .run(move || owner.replace_source(item, incoming))
                .await
                .expect("catalogue replacement reaches the Queue");
        }
        pcm.extend(rig.capture_frames(case, second, 128).await);
        if replace {
            assert!(gate.body_pending() > 0, "incoming body is still gated");
            let held = control.current().expect("playing while replacement waits");
            assert_eq!(held.id, item);
            assert_eq!(held.url, original);
            assert!(held.source_changing);
            assert!(control.position_seconds().expect("outgoing audio advances") > requested);
            gate.release_body();
        }
        if let HostTempo::Ramp(bpm) = tempo {
            rig.set_tempo(case, bpm, true).await;
        }
        let ArtifactSource::Value(model) = rig.provider_grid(0) else {
            panic!("authored beat grid");
        };
        let mut delivered = false;
        let mut applied = false;
        let mut frames = 5 * second;
        while frames > 0 {
            let previous = control.position_seconds().expect("outgoing mark");
            let block = frames.min(128);
            pcm.extend(rig.render(case, block).await);
            frames -= block;
            if replace && !applied {
                let current = control.current().expect("logical track through handover");
                assert_eq!(current.id, item);
                if current.url.as_deref() == Some(replacement.as_str()) {
                    assert!(!current.source_changing);
                    assert!(control.is_playing());
                    let advanced = control.position_seconds().expect("incoming mark") - previous;
                    assert!(advanced >= 0.0, "accepted source rewound by {advanced}s");
                    applied = true;
                } else {
                    assert!(
                        current.source_changing,
                        "Queue discarded the pending same-item source change: {current:?}"
                    );
                }
            }
            if replace
                && !delivered
                && std::iter::from_fn(|| events.try_recv().ok()).any(|envelope| {
                    matches!(envelope.event, QueueEvent::NextTrackReady { id, .. } if id == item)
                })
            {
                rig.host
                    .send(LinkedHostCommand::Grid {
                        deck: rig.decks[0].id(),
                        answer: GridAnswer {
                            item,
                            model: Ok((*model).clone()),
                        },
                    })
                    .await
                    .expect("known analysis reaches the incoming same-item track");
                delivered = true;
            }
        }
        assert!(
            !replace || applied,
            "same-item replacement did not commit within {frames_rendered} rendered frames: grid delivered={delivered}, current={:?}",
            control.current(),
            frames_rendered = 5 * second,
        );
        let current = control.current().expect("current track after the capture");
        assert_eq!(current.id, item);
        assert!(control.is_playing());
        assert!(!current.source_changing);
        assert_eq!(delivered, replace);
        if replace {
            assert_eq!(current.url.as_deref(), Some(replacement.as_str()));
        } else {
            assert_eq!(current.url, original);
        }
        let position = control.position_seconds().expect("canonical playhead");
        positions.push(position);
        let snapshot = rig.decks[0]
            .read_snapshot()
            .await
            .into_iter()
            .find(|(track, slot)| {
                track.track.item == item && slot.state == kithara_render::bridge::SlotState::Playing
            })
            .expect("sounding native track");
        assert!(matches!(
            snapshot.0.sync,
            SyncStatus::On | SyncStatus::Correcting { .. }
        ));
        assert!(rig.failures.is_empty(), "{:?}", rig.failures);
        drop(rig);
        captures.push(pcm);
    }
    let frames = (positions[1] - positions[0]).abs() * f64::from(case.sample_rate);
    let stems: Vec<_> = captures.iter().map(Vec::as_slice).collect();
    let failures = if let HostTempo::Track = tempo {
        synchronization_failures(
            "fixed HLS versus catalogue source continuation",
            &stems,
            CHANNELS,
            case.sample_rate,
            START_BPM,
        )
    } else {
        // Off the fixture tempo the beat period is the Host's, and a slew has
        // none at all: every fixture beat marker of the continued stem must
        // land on the frame of the same marker in the uninterrupted stem.
        let late = 3 * second;
        let [outgoing, continued] = [stems[0], stems[1]]
            .map(|stem| marked_rhythm_markers(stem, CHANNELS, case.sample_rate).0);
        let mut failures = Vec::new();
        if !outgoing.iter().any(|&marker| marker >= late) {
            failures.push(format!("no beat marker after frame {late}"));
        }
        if outgoing != continued {
            failures.push(format!(
                "beat markers differ: outgoing={outgoing:?}, continued={continued:?}"
            ));
        }
        failures
    };
    assert!(
        failures.is_empty() && frames <= 1.0,
        "PCM: {failures:?}, playheads={positions:?}, difference={frames} frames"
    );
}
