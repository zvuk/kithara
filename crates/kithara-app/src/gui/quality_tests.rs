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
