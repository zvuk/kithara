use std::convert::Infallible;

use ::kithara::{
    abr::AbrMode, events::TrackId, platform::sync::Arc, queue::TrackStatus,
    ui::render::ControlAction,
};
use kithara_app_library::{Playable, PlaybackChoice};
use kithara_integration_tests::{
    Content, CreatedHls, Delivery, FixtureBehavior, PrivateTestServer, TestServerHelper,
    packaged_ladder,
};
use kithara_test_fixtures::{SignalAsset, assets};
use kithara_test_utils::{kithara, off_thread::OffThread};

use super::rig::Rig;

const POSITION: &str = "deck.playback.position_secs@deck=a";
const PROGRESS: &str = "deck.playback.position_normalized@deck=a";
const PLAYING: &str = "deck.playback.playing@deck=a";
const READY: &str = "deck.stream.quality_ready@deck=a";

async fn ladder(server: &TestServerHelper) -> CreatedHls {
    server
        .create_hls(packaged_ladder())
        .await
        .expect("create the registered adaptive ladder")
}

fn drop_track(rig: &mut Rig, track: Playable) {
    rig.pump();
    rig.frame();
    rig.send("deck-a/drop", ControlAction::Record(track.into()));
    assert_eq!(rig.pump().len(), 1, "the drop queues one load");
}

fn playing_track(rig: &mut Rig) -> TrackId {
    rig.until(
        "playing at a nonzero position with the HLS ladder known",
        Rig::DEADLINE,
        Rig::frame,
        |rig| {
            rig.flag(PLAYING)
                && rig.scalar(POSITION) > 0.5
                && rig.scalar(PROGRESS) > 0.0
                && rig.queues[0]
                    .current_abr_handle()
                    .is_some_and(|abr| abr.variants().len() == 3)
        },
    );
    rig.queues[0].current().expect("the playing track").id
}

fn select(rig: &mut Rig, row: &str) {
    rig.send("deck-a/stream/cell", ControlAction::Activate);
    rig.send(
        &format!("deck-a/stream/{row}/pick"),
        ControlAction::Activate,
    );
    assert_eq!(rig.apply().len(), 1, "{row} queues one quality command");
}

fn presentation(rig: &Rig, track: TrackId, position: &mut f64) {
    let queue = &rig.queues[0];
    assert_eq!(queue.current().expect("the playing track").id, track);
    let engine = queue.playback_view();
    let shown = rig.scalar(POSITION);
    assert!(
        engine.playing && rig.flag(PLAYING),
        "quality selection interrupted playing"
    );
    assert_eq!(
        Some(shown),
        engine.position,
        "the GUI shows the consumed position"
    );
    assert!(
        shown >= *position && rig.scalar(PROGRESS) > 0.0,
        "playhead regressed: previous={position}, shown={shown}, duration={:?}, progress={}",
        engine.duration,
        rig.scalar(PROGRESS),
    );
    *position = shown;
}

#[kithara::test(native, tokio)]
async fn quality_switch_keeps_hls_playhead_and_applies_each_rung() {
    let server = PrivateTestServer::start().await;
    let url = ladder(&server.helper()).await.master_url();
    let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::playback()))
        .await
        .expect("the offline rig starts");
    rig.call(move |rig| {
        drop_track(rig, Playable::new(url.into()));
        let track = playing_track(rig);
        let abr = rig.queues[0].current_abr_handle().expect("the HLS ladder");
        let from = abr
            .current_variant()
            .expect("the sounding rung")
            .variant_index
            .get();
        let rungs = abr.variants();
        let mut position = rig.scalar(POSITION);
        for slot in [(from + 1) % rungs.len(), from] {
            let row = format!("variant-{slot}");
            select(rig, &row);
            rig.until(
                "the selected HLS rung becomes current",
                Rig::DEADLINE,
                Rig::frame,
                |rig| {
                    presentation(rig, track, &mut position);
                    abr.current_variant()
                        .is_some_and(|v| v.variant_index == rungs[slot].variant_index)
                },
            );
            assert_eq!(abr.mode(), Some(AbrMode::Manual(rungs[slot].variant_index)));
            assert!(rig.flag(&format!("deck.stream.variant_active@deck=a,variant={slot}")));
        }
        select(rig, "auto");
        rig.until(
            "Auto resumes with continuing playback",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                presentation(rig, track, &mut position);
                matches!(abr.mode(), Some(AbrMode::Auto(_)))
                    && rig.flag("deck.stream.variant_active@deck=a,variant=auto")
            },
        );
    })
    .await;
    rig.close().await;
}

#[kithara::test(native, tokio)]
async fn quality_menu_keeps_known_catalogue_choices_during_handover() {
    let server = PrivateTestServer::start().await;
    let helper = server.helper();
    let adaptive = ladder(&helper).await.master_url();
    let asset =
        assets::by_name(SignalAsset::MP3_SINE880_30S.name()).expect("the progressive fixture");
    let gate = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(asset.bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Gated { after_bytes: 0 },
    });
    let first = gate.child_url("manual.mp3");
    let second = helper.signal(SignalAsset::MP3_CLICKS126_30S);
    let mut playable = Playable::new(adaptive.to_string());
    playable.identity = Some("test:quality-track".to_owned());
    playable.choices = [
        (adaptive, "Auto", true),
        (first, "MP3", false),
        (second, "MP3 Alternate", false),
    ]
    .into_iter()
    .map(|(source, label, adaptive)| PlaybackChoice {
        source,
        label: label.to_owned(),
        adaptive,
    })
    .collect();
    let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::playback()))
        .await
        .expect("the offline rig starts");
    rig.call(move |rig| {
        drop_track(rig, playable);
        let track = playing_track(rig);
        let mut position = rig.scalar(POSITION);
        let requested_at = position;
        select(rig, "variant-0");
        rig.until(
            "outgoing playback advances while the replacement is withheld",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                presentation(rig, track, &mut position);
                position >= requested_at + 0.2
                    && gate.body_pending() > 0
                    && rig.ui.snapshot.lettered("a").expect("deck A").stream.busy
            },
        );
        let choices_remained_visible = rig.flag(READY)
            && !rig.flag("deck.stream.auto_hidden@deck=a")
            && !rig.flag("deck.stream.variant_hidden@deck=a,variant=0")
            && !rig.flag("deck.stream.variant_hidden@deck=a,variant=1");
        gate.release_body();
        assert!(
            choices_remained_visible,
            "the stored catalogue choices disappeared during a handover"
        );
    })
    .await;
    rig.close().await;
}

#[kithara::test(native, tokio)]
async fn quality_menu_loads_only_while_the_initial_hls_ladder_is_unknown() {
    let server = PrivateTestServer::start().await;
    let helper = server.helper();
    let ladder = ladder(&helper).await;
    let gates: Vec<_> = (0..3)
        .map(|slot| helper.register_media_playlist_gate(ladder.token(), slot))
        .collect();
    let url = ladder.master_url();
    let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::playback()))
        .await
        .expect("the offline rig starts");
    rig.call(move |rig| {
        drop_track(rig, Playable::new(url.into()));
        rig.until(
            "the first HLS load is waiting on its playlists",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                let deck = rig.ui.snapshot.lettered("a").expect("deck A");
                deck.tracks
                    .first()
                    .is_some_and(|track| track.status == TrackStatus::Loading)
                    && deck.stream.variants.is_empty()
                    && gates.iter().any(|gate| gate.requested() > 0)
            },
        );
        let pending_is_visible = !rig.flag("deck.stream.quality_hidden@deck=a")
            && !rig.flag(READY)
            && rig.flag("deck.stream.auto_hidden@deck=a");
        drop(gates);
        playing_track(rig);
        assert!(
            pending_is_visible,
            "the menu shows Loading while the first ladder is unknown"
        );
        assert!(rig.flag(READY), "the known ladder replaces Loading");
        assert!(!rig.flag("deck.stream.variant_hidden@deck=a,variant=0"));
    })
    .await;
    rig.close().await;
}

#[kithara::test(native, tokio)]
async fn a_progressive_initial_load_keeps_the_quality_menu_hidden() {
    let server = PrivateTestServer::start().await;
    let gate = server.helper().register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(assets::rhythm_mp3_deck_a_120bpm_48k().bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Gated { after_bytes: 0 },
    });
    let url = gate.child_url("progressive.mp3");
    let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::playback()))
        .await
        .expect("the offline rig starts");
    rig.call(move |rig| {
        drop_track(rig, Playable::new(url.into()));
        rig.until(
            "the progressive file is loading",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                rig.ui
                    .snapshot
                    .lettered("a")
                    .expect("deck A")
                    .tracks
                    .first()
                    .is_some_and(|track| track.status == TrackStatus::Loading)
                    && gate.body_pending() > 0
            },
        );
        assert!(rig.flag("deck.stream.quality_hidden@deck=a"));
        assert!(
            rig.flag(READY),
            "progressive files do not load a quality ladder"
        );
        gate.release_body();
    })
    .await;
    rig.close().await;
}

enum QualityCheck {
    Extent,
    Position,
    Pause,
}

#[kithara::test(native, tokio, flash(false))]
#[case::logical_duration(QualityCheck::Extent)]
#[case::playhead(QualityCheck::Position)]
#[case::pause_during_preparation(QualityCheck::Pause)]
async fn catalogue_quality_switch_preserves_the_sounding_track(#[case] check: QualityCheck) {
    let server = PrivateTestServer::start().await;
    let helper = server.helper();
    let init = Arc::new(
        assets::rhythm_fmp4_init_deck_a_120bpm_48k()
            .bytes()
            .to_vec(),
    );
    let media = Arc::new(
        assets::rhythm_fmp4_media_deck_a_120bpm_48k()
            .bytes()
            .to_vec(),
    );
    let adaptive = helper
        .create_hls(
            kithara_integration_tests::HlsFixtureBuilder::new()
                .variant_count(3)
                .segments_per_variant(1)
                .segment_duration_secs(12.0)
                .segment_size(media.len())
                .codecs("fLaC".to_owned())
                .init_data_per_variant(vec![init; 3])
                .custom_data(media),
        )
        .await
        .expect("register the rhythmic HLS version")
        .master_url();
    let gate = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(assets::rhythm_mp3_deck_a_120bpm_48k().bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Gated { after_bytes: 0 },
    });
    let first = gate.child_url("manual.mp3");
    let second = helper
        .register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(assets::rhythm_wav_deck_a_120bpm_48k().bytes().to_vec()),
                content_type: Some("audio/wav"),
            },
            delivery: Delivery::Range,
        })
        .child_url("lossless.wav");
    let mut playable = Playable::new(adaptive.to_string());
    playable.identity = Some("test:rhythmic-track".to_owned());
    playable.choices = [
        (adaptive.clone(), "Auto", true),
        (first.clone(), "MP3", false),
        (second.clone(), "Lossless", false),
    ]
    .into_iter()
    .map(|(source, label, adaptive)| PlaybackChoice {
        source,
        label: label.to_owned(),
        adaptive,
    })
    .collect();

    let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::playback()))
        .await
        .expect("the offline rig starts");
    rig.call(move |rig| {
        drop_track(rig, playable);
        let track = playing_track(rig);
        let queue = rig.queues[0].clone();
        let mut position = rig.scalar(POSITION);
        let requested_at = position;
        let duration = queue
            .playback_view()
            .duration
            .expect("the HLS extent is known");
        let expected_controls = queue.warp_controls();
        let expected_rate = queue.default_rate();

        select(rig, "variant-0");
        rig.until(
            "the original source plays while replacement bytes are withheld",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                presentation(rig, track, &mut position);
                position >= requested_at + 0.2 && gate.body_pending() > 0
            },
        );
        let before_release = position;
        if matches!(check, QualityCheck::Pause) {
            queue.pause();
            rig.until(
                "the pause keeps the accepted source and cancels its pending replacement",
                Rig::DEADLINE,
                Rig::frame,
                |rig| {
                    let current = queue.current().expect("the paused track remains current");
                    assert_eq!(current.id, track);
                    assert_eq!(current.url.as_deref(), Some(adaptive.as_str()));
                    !current.source_changing && !rig.flag(PLAYING)
                },
            );
            assert!(rig.scalar(POSITION) >= before_release);
            let paused_at = rig.scalar(POSITION);
            gate.release_body();
            rig.pump();
            rig.frame();
            assert_eq!(rig.scalar(POSITION), paused_at);
            assert_eq!(
                queue.current().unwrap().url.as_deref(),
                Some(adaptive.as_str())
            );
            return;
        }
        gate.release_body();
        let extent_only = matches!(check, QualityCheck::Extent);
        for (row, expected) in [
            ("variant-0", first),
            ("variant-1", second),
            ("auto", adaptive),
        ] {
            if row != "variant-0" {
                select(rig, row);
            }
            rig.until(
                "the named source is accepted on the same playing track",
                Rig::DEADLINE,
                Rig::frame,
                |rig| {
                    let current = queue.current().expect("the logical track stays current");
                    let view = queue.playback_view();
                    if extent_only {
                        assert_eq!(current.id, track);
                        assert!(view.playing && rig.flag(PLAYING));
                        assert!(
                            view.duration.is_some(),
                            "known logical duration disappeared at {current:?}, position={:?}",
                            view.position,
                        );
                    } else {
                        presentation(rig, track, &mut position);
                    }
                    assert_eq!(queue.warp_controls(), expected_controls);
                    assert_eq!(queue.default_rate(), expected_rate);
                    current.url.as_deref() == Some(expected.as_str())
                        && !current.source_changing
                        && rig.flag(&format!(
                            "deck.stream.variant_active@deck=a,variant={}",
                            row.strip_prefix("variant-").unwrap_or(row)
                        ))
                },
            );
        }
        assert!(
            extent_only || (position > before_release && position < duration),
            "the rendered track advances through all three accepted sources"
        );
    })
    .await;
    rig.close().await;
}
