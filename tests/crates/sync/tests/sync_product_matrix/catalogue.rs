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
    Audible, CHANNELS, HlsProtection, ONE_DECK, ProductHarness, Provider, START_BPM,
    prepared_sources,
};

#[kithara::test(native, tokio, multi_thread, serial, timeout(Long))]
#[case::constant_sync(None)]
#[case::during_a_tempo_ramp(Some(124.0))]
async fn same_item_catalogue_replacement_keeps_the_linked_pcm_phase(#[case] tempo: Option<f64>) {
    let case = ONE_DECK;
    let second = usize::try_from(case.sample_rate).expect("fixture sample rate");
    let sources = prepared_sources(Provider::HlsSame(HlsProtection::Plain)).await;
    let mut captures = Vec::new();
    let mut positions = Vec::new();
    for replace in [false, true] {
        let mut rig =
            ProductHarness::new(case, &sources, Start::Seconds(0.0), Audible::Deck(0)).await;
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
        if let Some(bpm) = tempo {
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
                if current.url.as_deref() == Some(replacement.as_str()) {
                    assert_eq!(current.id, item);
                    assert!(control.is_playing());
                    let advanced = control.position_seconds().expect("incoming mark") - previous;
                    assert!(advanced >= 0.0, "accepted source rewound by {advanced}s");
                    applied = true;
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
        let current = control.current().expect("current track after the capture");
        assert_eq!(current.id, item);
        assert!(control.is_playing());
        assert!(!current.source_changing);
        assert_eq!(delivered, replace);
        assert_eq!(applied, replace);
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
    let failures = if tempo.is_some() {
        // A slewing tempo has no constant beat period: every fixture beat
        // marker of the continued stem must land on the frame of the same
        // marker in the uninterrupted stem, through the slew itself.
        let slew = 3 * second;
        let [outgoing, continued] = [stems[0], stems[1]]
            .map(|stem| marked_rhythm_markers(stem, CHANNELS, case.sample_rate).0);
        let mut failures = Vec::new();
        if !outgoing.iter().any(|&marker| marker >= slew) {
            failures.push(format!(
                "no beat marker after the tempo command at frame {slew}"
            ));
        }
        if outgoing != continued {
            failures.push(format!(
                "beat markers differ: outgoing={outgoing:?}, continued={continued:?}"
            ));
        }
        failures
    } else {
        synchronization_failures(
            "fixed HLS versus catalogue source continuation",
            &stems,
            CHANNELS,
            case.sample_rate,
            START_BPM,
        )
    };
    assert!(
        failures.is_empty() && frames <= 1.0,
        "PCM: {failures:?}, playheads={positions:?}, difference={frames} frames"
    );
}
