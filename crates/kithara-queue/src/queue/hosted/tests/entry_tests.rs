pub(super) use kithara_assets::{AssetStore, StorageBackend};
use kithara_command::{Live, Port};
pub(super) use kithara_command::{Outcome, Rejection, ScopedReceipt, Seq, When};
use kithara_config::ConfigOwner;
pub(super) use kithara_events::TrackId;
pub(super) use kithara_platform::time::Duration;
pub(super) use kithara_play::{
    DeckMixSettings, DeckMixerConfig, DeckPart, DeckPass, Outbox, PlayWorker, PlayWorkerConfig,
    Player, ResourceConfig, ResourcePrep, ResourceSrc, TrackReceipt, TrackStatus as PlayingStatus,
    mock,
};
use kithara_play::{DeckMixSettingsChange, DeckProtocol, PlayerEvent, Position};
pub(super) use kithara_render::{
    LaneFrame,
    bridge::{Returned, SlotMark},
};
pub(super) use kithara_signal::{AudioChunkInfo, AudioSpec, FrameCount, SessionFrame};
pub(super) use kithara_test_utils::{TestTempDir, kithara};

use super::*;
pub(super) use crate::{
    QueueConfig, TrackSource,
    test_pools::{TestPools, pools},
};

#[kithara::test(native)]
fn rejected_live_commands_leave_requested_values_unchanged() {
    use crate::{CrossfadeSettings, QueueConfigPatch, QueueSettings, QueueSettingsChange};

    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let settings = QueueSettings::builder()
        .crossfade(CrossfadeSettings {
            duration: 1.0,
            ..CrossfadeSettings::default()
        })
        .build();
    let mut queue = Queue::new(QueueConfig::builder().prep(prep).settings(settings).build());
    let mut rig = mock::DeckRig::new(DeckMixerConfig::default()).expect("deck rig");
    let change = QueueSettingsChange::Crossfade(CrossfadeSettings {
        duration: 2.0,
        ..CrossfadeSettings::default()
    });

    assert!(matches!(
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::apply(
                queue,
                QueueCommand::ConfigureQueue(change, When::At(SessionFrame::new(128))),
                out,
            )
        }),
        Err(PlayError::Untimed)
    ));
    assert_eq!(queue.config.settings.crossfade().duration, 1.0);
    assert_eq!(queue.control().crossfade_settings().duration, 1.0);

    let Err(error) = serde_yaml_ng::from_str::<QueueConfigPatch>("preload_lead: 5.0\n") else {
        panic!("immutable preload lead must not be a document key");
    };
    assert!(error.to_string().contains("preload_lead"));
    assert_eq!(queue.config.preload_lead.as_secs_f32(), 3.5);

    assert!(
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::apply(queue, QueueCommand::ConfigureQueue(change, When::Next), out)
        })
        .is_ok()
    );
    assert_eq!(queue.config.settings.crossfade().duration, 2.0);
    assert_eq!(queue.control().crossfade_settings().duration, 2.0);
}

fn mix_command(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    mix: &mut Live<DeckMixSettings, DeckProtocol>,
    command: QueueCommand<TestPools>,
) -> Result<Option<Seq>, PlayError> {
    let output = mock::output(None).get();
    let deck = queue.deck.mixer.clone();
    let pass = DeckPass {
        mix: *mix.config(),
        suspended: false,
        now: SessionFrame::new(0),
        delivery: FrameCount::new(128),
        output: &output,
        deck: &deck,
    };
    let mut scope = rig.ring.scope(rig.scope).expect("deck scope");
    let mut out = Outbox::new(&mut scope, &mut rig.dispatcher)
        .in_pass(pass)
        .lend_mix(mix);
    HostedDeck::tick(queue, pass, &mut out);
    Player::apply(queue, command, &mut out)
}

#[kithara::test(native)]
fn rejected_slot_volume_leaves_live_values_unchanged() {
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let mut queue = Queue::new(QueueConfig::builder().prep(prep).build());
    let control = queue.control();
    let mut events = queue.subscribe::<PlayerEvent>();
    let mut mix = Live::<DeckMixSettings, DeckProtocol>::new(DeckMixSettings::default())
        .expect("default mix");
    let mut rig = mock::DeckRig::new(DeckMixerConfig::default()).expect("deck rig");
    {
        let mut scope = rig.ring.scope(rig.scope).expect("deck scope");
        let capacity = scope.available();
        assert!(capacity > 0);
        for _ in 0..capacity {
            scope
                .send(
                    When::Next,
                    kithara_command::Batch {
                        basis: Vec::new(),
                        commands: vec![DeckPart::Mix(DeckMixSettingsChange::Muted(false))],
                    },
                )
                .expect("fill scoped channel");
        }
        assert_eq!(scope.available(), 0);
    }
    assert!(matches!(
        mix_command(&mut queue, &mut rig, &mut mix, QueueCommand::SetVolume(0.4)),
        Err(PlayError::Full("deck"))
    ));
    assert_eq!(f32::from(mix.config().volume()), 1.0);
    assert_eq!(f32::from(mix.projected().volume()), 1.0);
    assert_eq!(control.volume(), 1.0);
    assert!(matches!(
        mix_command(&mut queue, &mut rig, &mut mix, QueueCommand::SetMuted(true)),
        Err(PlayError::Full("deck"))
    ));
    assert!(!mix.config().muted());
    assert!(!mix.projected().muted());
    assert!(!control.is_muted());
    assert!(events.try_recv().is_err());

    rig.ring.close(rig.scope).expect("close scoped channel");
    assert!(matches!(
        mix_command(&mut queue, &mut rig, &mut mix, QueueCommand::SetVolume(0.4)),
        Err(PlayError::Closed)
    ));
    assert_eq!(f32::from(mix.config().volume()), 1.0);
    assert_eq!(f32::from(mix.projected().volume()), 1.0);
    assert_eq!(control.volume(), 1.0);
    assert!(matches!(
        mix_command(&mut queue, &mut rig, &mut mix, QueueCommand::SetMuted(true)),
        Err(PlayError::Closed)
    ));
    assert!(!mix.config().muted());
    assert!(!mix.projected().muted());
    assert!(!control.is_muted());
    assert!(events.try_recv().is_err());

    let mut idle = mock::DeckRig::new(DeckMixerConfig::default()).expect("idle deck rig");
    let seq = mix_command(
        &mut queue,
        &mut idle,
        &mut mix,
        QueueCommand::SetVolume(0.4),
    )
    .expect("idle volume accepted")
    .expect("mix batch");
    assert_eq!(f32::from(mix.config().volume()), 1.0);
    assert_eq!(f32::from(mix.projected().volume()), 0.4);
    assert!(events.try_recv().is_err());
    let receipts = idle
        .block(SessionFrame::new(0), 0.0)
        .expect("idle mix applied");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].seq(), seq);
    assert!(matches!(receipts[0].outcome(), Outcome::Applied { .. }));
    assert!(mix.settle(&receipts[0]).is_some());
    assert_eq!(f32::from(mix.config().volume()), 0.4);
    let output = mock::output(None).get();
    let deck = queue.deck.mixer.clone();
    let pass = DeckPass {
        mix: *mix.config(),
        suspended: false,
        now: SessionFrame::new(0),
        delivery: FrameCount::new(128),
        output: &output,
        deck: &deck,
    };
    let mut scope = idle.ring.scope(idle.scope).expect("idle scope");
    let mut out = Outbox::new(&mut scope, &mut idle.dispatcher).in_pass(pass);
    HostedDeck::tick(&mut queue, pass, &mut out);
    assert_eq!(control.volume(), 0.4);
    assert!(matches!(events.try_recv().map(|event| event.event),
        Ok(PlayerEvent::VolumeChanged { volume }) if volume == 0.4));
    assert!(
        events.try_recv().is_err(),
        "idle volume emits exactly one event"
    );
}

#[kithara::flash(io)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn arm_current_fixture(
    worker: &PlayWorker<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    dir: &TestTempDir,
) -> kithara_render::Loaded<kithara_play::OpenedTrack> {
    let mut loaded = std::pin::pin!(loaded_fixture(worker, rig, dir));
    loop {
        hang_tick!();
        if let Ok(loaded) =
            kithara_platform::time::timeout(Duration::from_millis(10), loaded.as_mut()).await
        {
            hang_reset!();
            return loaded;
        }
    }
}

#[kithara::test(tokio)]
async fn finish_disarms_and_ignores_stale_ticket() {
    let (mut queue, id, _, mut rig, dir) = pending_selection();
    let stale = rig
        .open(Err(kithara_play::LoadRefusal::Cancelled))
        .expect("old terminal dispatcher receipt");
    with_outbox(&mut queue, &mut rig, Queue::cancel_target).expect("cancel old target");
    let current_seq = with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Select {
                id,
                transition: crate::Transition::None,
            },
            out,
        )
    })
    .expect("select replacement")
    .expect("replacement open");
    assert_ne!(stale.seq(), current_seq);
    let current = queue.tracks.records()[0]
        .load
        .as_ref()
        .expect("replacement guard")
        .token()
        .clone();
    let watch = queue.tracks.records()[0]
        .load
        .as_ref()
        .expect("replacement guard")
        .watch()
        .clone();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::settle(queue, TrackReceipt::Loaded(stale), out)
    });
    assert_eq!(
        queue.tracks.records()[0].status,
        TrackStatus::Loading,
        "stale finish must not evict"
    );
    assert!(
        queue.tracks.records()[0].load.is_some(),
        "stale finish must retain the current guard"
    );
    queue
        .tracks
        .apply_report(crate::loading::LoadReport::Slow { id, watch });
    assert_eq!(queue.tracks.records()[0].status, TrackStatus::Slow);
    assert!(!current.is_cancelled());
    let loaded = arm_current_fixture(queue.worker().expect("fixture worker"), &mut rig, &dir).await;
    answer_loaded(&mut queue, &mut rig, loaded);
    assert!(
        queue.tracks.records()[0].load.is_none(),
        "finished attempt must leave the record vacant"
    );
    assert!(!current.is_cancelled());
    with_outbox(&mut queue, &mut rig, Queue::cancel_target).expect("release completed target");
    let next = with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Select {
                id,
                transition: crate::Transition::None,
            },
            out,
        )
    })
    .expect("begin following attempt")
    .expect("following open");
    assert_ne!(next, current_seq);
    assert!(queue.tracks.records()[0].load.is_some());
    assert!(
        !queue.tracks.records()[0]
            .load
            .as_ref()
            .expect("following guard")
            .token()
            .is_cancelled()
    );
}

async fn selected_source_clock() -> (
    Queue<TestPools>,
    mock::DeckRig<TestPools>,
    TestTempDir,
    AudioChunkInfo,
) {
    let (mut queue, _, _, mut rig, dir) = pending_selection();
    let seq = queue
        .active
        .iter()
        .find_map(|active| active.load)
        .expect("selected load")
        .seq();
    let loaded =
        crate::mock::loaded_request(queue.worker().expect("fixture worker"), &mut rig, seq).await;
    let clock = loaded
        .opened
        .pcm
        .next_chunk_info()
        .expect("selected source's admitted PCM");
    assert!(
        clock.media_origin.is_some() && clock.visible_origin.is_some(),
        "the selected WAV publishes its original source clock"
    );
    answer_loaded(&mut queue, &mut rig, loaded);
    crate::mock::finish(&mut queue, &mut rig);
    (queue, rig, dir, clock)
}

#[kithara::test(tokio)]
async fn only_the_accepted_seek_receipt_relinquishes_the_requested_position() {
    let (mut queue, mut rig, _dir, clock) = selected_source_clock().await;
    let before = queue.control().position_seconds();
    let first = with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Seek {
                to: Position::try_from_secs_f64(1.0).expect("first seek"),
            },
            out,
        )
    })
    .expect("accept first seek")
    .expect("first seek receipt");
    let second = with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Seek {
                to: Position::try_from_secs_f64(2.0).expect("second seek"),
            },
            out,
        )
    })
    .expect("accept second seek")
    .expect("second seek receipt");
    assert_eq!(queue.control().position_seconds(), Some(2.0));
    let slot = queue
        .current_track()
        .expect("selected track")
        .snapshot()
        .slot
        .expect("selected slot");
    let outcome = Outcome::Applied {
        at: SessionFrame::new(256),
        data: (),
    };
    let mut batch = kithara_command::Batch::<DeckProtocol> {
        basis: vec![(slot, None)],
        commands: Vec::new(),
    };
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq: first,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    queue.publish();
    assert_eq!(queue.control().position_seconds(), Some(2.0));
    let receipt = rig
        .block(SessionFrame::new(256), 0.0)
        .expect("seek adoption block")
        .into_iter()
        .find(|receipt| receipt.seq() == second)
        .expect("accepted seek adoption");
    let (_, applied) = receipt.into();
    let segment = applied
        .commands
        .iter()
        .find_map(|part| match part {
            DeckPart::Adopt { segment, .. } => Some(*segment),
            _ => None,
        })
        .expect("seek segment");
    batch.commands = applied.commands;
    let raw_position = clock
        .media_origin
        .expect("selected source's media zero")
        .checked_add(Duration::from_secs_f64(before.expect("fixture position")))
        .expect("fixture raw position");
    let snapshot = &mut queue.deck.mixer.slots[usize::from(slot.get())];
    snapshot.source = Some(AudioChunkInfo {
        spec: clock.spec,
        segment,
        source_revision: 0,
        media_origin: clock.media_origin,
        visible_origin: clock.visible_origin,
        media_end: clock.media_end,
        ..AudioChunkInfo::default()
    });
    snapshot.position = raw_position.as_secs_f64();
    snapshot.mark = Some(SlotMark {
        session: SessionFrame::new(256),
        lane: LaneFrame { segment, frame: 0 },
        position: raw_position,
    });
    with_outbox(&mut queue, &mut rig, |queue, out| {
        let current = queue.active_current_index().expect("selected track");
        queue
            .active
            .get_mut(current)
            .expect("selected track")
            .track
            .tick(SessionFrame::new(256), out);
    });
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq: second,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    queue.publish();
    assert_eq!(queue.control().position_seconds(), before);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn an_applied_seek_keeps_its_target_until_the_matching_render_mark() {
    let (mut queue, mut rig, _dir, clock) = selected_source_clock().await;
    let requested = Position::try_from_secs_f64(2.0).expect("requested seek");
    let sent = with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Seek { to: requested }, out)
    })
    .expect("accept seek")
    .expect("seek receipt");
    let receipt = rig
        .block(SessionFrame::new(256), 0.0)
        .expect("adoption block")
        .into_iter()
        .find(|receipt| receipt.seq() == sent)
        .expect("seek adoption");
    let (outcome, mut batch) = receipt.into();
    let segment = batch
        .commands
        .iter()
        .find_map(|part| match part {
            DeckPart::Adopt { segment, .. } => Some(*segment),
            _ => None,
        })
        .expect("adopted segment");
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq: sent,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    assert_eq!(queue.control().position_seconds(), Some(2.0));
    assert_eq!(queue.control().playback_view().position, Some(2.0));
    let slot = queue
        .current_track()
        .expect("current track")
        .snapshot()
        .slot
        .expect("current slot");
    let origin = clock.media_origin.expect("selected source's media zero");
    let raw_position = origin
        .checked_add(Duration::from_secs_f64(0.25))
        .expect("old fixture raw position");
    let snapshot = &mut queue.deck.mixer.slots[usize::from(slot.get())];
    snapshot.source = Some(AudioChunkInfo {
        spec: clock.spec,
        segment: kithara_signal::SegmentId::FIRST,
        source_revision: 0,
        media_origin: clock.media_origin,
        visible_origin: clock.visible_origin,
        media_end: clock.media_end,
        ..AudioChunkInfo::default()
    });
    snapshot.position = raw_position.as_secs_f64();
    snapshot.mark = Some(SlotMark {
        session: SessionFrame::new(256),
        lane: LaneFrame {
            segment: kithara_signal::SegmentId::FIRST,
            frame: 0,
        },
        position: raw_position,
    });
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::tick(queue, SessionFrame::new(256), out);
    });
    assert_eq!(queue.control().position_seconds(), Some(2.0));
    let raw_position = origin
        .checked_add(Duration::from_secs_f64(2.25))
        .expect("matching fixture raw position");
    queue.deck.mixer.slots[usize::from(slot.get())].position = raw_position.as_secs_f64();
    queue.deck.mixer.slots[usize::from(slot.get())]
        .source
        .as_mut()
        .expect("declared fixture source")
        .segment = segment;
    queue.deck.mixer.slots[usize::from(slot.get())].mark = Some(SlotMark {
        session: SessionFrame::new(384),
        lane: LaneFrame { segment, frame: 0 },
        position: raw_position,
    });
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::tick(queue, SessionFrame::new(384), out);
    });
    assert_eq!(queue.control().position_seconds(), Some(2.25));
    assert_eq!(queue.control().playback_view().position, Some(2.25));
}

#[kithara::test(tokio)]
async fn atomic_track_id_cas_arm_then_disarm() {
    let (mut queue, mut rig, dir) = empty_queue();
    queue.config.action_at_item_end = ActionAtItemEnd::None;
    let source = TrackSource::Uri(
        dir.path()
            .join("entry.wav")
            .to_str()
            .expect("fixture path")
            .to_owned(),
    );
    with_outbox(&mut queue, &mut rig, |queue, out| {
        for id in [TrackId(2), TrackId(3), TrackId(4)] {
            Player::apply(
                queue,
                QueueCommand::Append {
                    position: Position::ZERO,
                    id,
                    source: source.clone(),
                },
                out,
            )
            .expect("append arm fixture");
        }
        Player::apply(
            queue,
            QueueCommand::Select {
                id: TrackId(2),
                transition: crate::Transition::None,
            },
            out,
        )
        .expect("select sounding predecessor");
    });
    let loaded = arm_current_fixture(queue.worker().expect("fixture worker"), &mut rig, &dir).await;
    let source = loaded
        .opened
        .pcm
        .next_chunk_info()
        .expect("decoded predecessor geometry");
    assert!(source.media_end.is_some(), "decoded predecessor endpoint");
    answer_loaded(&mut queue, &mut rig, loaded);
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(TrackId(2)));
    let slot = queue
        .current_track()
        .expect("sounding predecessor")
        .snapshot()
        .slot
        .expect("seated predecessor");
    queue.deck.mixer.slots[usize::from(slot.get())].source = Some(source);
    queue.deck.mixer.slots[usize::from(slot.get())].mark = Some(SlotMark {
        session: SessionFrame::new(256),
        lane: LaneFrame {
            segment: kithara_signal::SegmentId::FIRST,
            frame: 0,
        },
        position: Duration::ZERO,
    });
    with_outbox(&mut queue, &mut rig, |queue, out| {
        let current = queue.active_current_index().expect("sounding predecessor");
        queue
            .active
            .get_mut(current)
            .expect("current entry")
            .track
            .tick(SessionFrame::new(256), out);
    });
    assert!(
        queue.current_end().is_some(),
        "deadline guard must see a sounding predecessor"
    );
    let seq = with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Select {
                id: TrackId(3),
                transition: crate::Transition::None,
            },
            out,
        )
    })
    .expect("arm track three")
    .expect("open sequence");
    let incoming = queue.incoming_index(TrackId(3)).expect("armed track");
    queue.active.get_mut(incoming).expect("incoming slot").role = Role::Incoming;
    queue.pending_handover = Some(super::super::super::types::PendingHandover {
        seq,
        item: TrackId(3),
        slot: queue.active.get(incoming).expect("incoming slot").slot,
        replacement: queue.active.is_replacement(incoming),
        reason: crate::AdvanceReason::UserSelect,
    });
    let ids = queue.track_ids();
    let cursor = queue.navigation_cursor();
    assert_eq!(
        queue.navigation.next(&ids, cursor, true, false),
        Some(TrackId(4))
    );
    let unrelated = with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.load_track(
            TrackId(4),
            Role::Preloaded,
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect("unrelated preload")
    .expect("distinct open sequence");
    assert_ne!(seq, unrelated);
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::SetActionAtItemEnd(ActionAtItemEnd::Advance),
            out,
        )
        .expect("enable automatic admission with a pending target");
        for _ in 0..2 {
            queue
                .tick_deadlines(SessionFrame::new(256), Some(&mock::output(None).get()), out)
                .expect("deadline admission while track three is armed");
        }
    });
    assert_eq!(
        queue.target.map(|target| target.to),
        Some(TrackId(3)),
        "a second arm must not replace the armed track"
    );
    queue
        .transition_applied(unrelated, incoming, SessionFrame::new(384))
        .expect("unrelated receipt");
    assert_eq!(queue.target.map(|target| target.to), Some(TrackId(3)));
    assert_eq!(queue.current, Some(TrackId(2)));
    queue
        .transition_applied(seq, incoming, SessionFrame::new(384))
        .expect("matching receipt");
    assert_eq!(queue.current, Some(TrackId(3)));
    assert!(queue.target.is_none());
}

#[kithara::test]
fn a_host_pass_publishes_the_deck_mixer_counters() {
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let mut queue = Queue::new(QueueConfig::builder().prep(prep).build());
    let control = queue.control();
    assert_eq!(control.mixer_blocks(), 0);
    let mut deck = kithara_play::DeckSnapshot::new(DeckMixerConfig::default());
    deck.blocks = 7;
    let output = mock::output(None).get();
    let pass = DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: SessionFrame::new(0),
        delivery: FrameCount::new(128),
        output: &output,
        deck: &deck,
    };
    let mut rig = mock::DeckRig::new(DeckMixerConfig::default()).expect("deck rig");
    let mut scope = rig.ring.scope(rig.scope).expect("deck scope");
    let mut out = Outbox::new(&mut scope, &mut rig.dispatcher).in_pass(pass);
    HostedDeck::tick(&mut queue, pass, &mut out);
    assert_eq!(control.mixer_blocks(), 7);
    assert_eq!(control.mixer_metrics(), deck.metrics);
}

#[kithara::test(tokio)]
async fn an_item_is_loaded_on_its_load_receipt_before_its_attach_applies() {
    let (mut queue, first, _, mut rig, dir) = pending_selection();
    let mut events = queue.subscribe::<QueueEvent>();
    answer_load(&mut queue, &mut rig, &dir).await;
    assert_eq!(
        queue.track(first).expect("record").status,
        TrackStatus::Loaded
    );
    assert_eq!(
        queue
            .track(first)
            .expect("record")
            .metadata()
            .title
            .as_deref(),
        Some("Loaded fixture")
    );
    assert!(queue.current_track().is_none());
    assert_eq!(
        queue
            .tracks_active()
            .next()
            .expect("incoming track")
            .snapshot()
            .status,
        PlayingStatus::Loading
    );
    let mut ready = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|envelope| matches!(envelope.event, QueueEvent::NextTrackReady { id, .. } if id == first)).count();
    assert_eq!(ready, 1);
    answer_deck(&mut queue, &mut rig, SessionFrame::new(0));
    ready += std::iter::from_fn(|| events.try_recv().ok())
        .filter(|envelope| matches!(envelope.event, QueueEvent::NextTrackReady { id, .. } if id == first)).count();
    assert_eq!(
        queue.track(first).expect("record").status,
        TrackStatus::Loaded
    );
    assert_eq!(ready, 1);
}

#[kithara::test(tokio)]
async fn a_pause_before_the_selected_item_enters_makes_it_current_paused() {
    let (mut queue, first, _, mut rig, dir) = pending_selection();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Pause { at: When::Next }, out)
    })
    .expect("pause pending selection");
    answer_load(&mut queue, &mut rig, &dir).await;
    assert_eq!(queue.current, None);
    crate::mock::settle(&mut queue, &mut rig, SessionFrame::new(0));
    assert_eq!(queue.current, None);
    let Some(crate::queue::types::Continuation::Pause(pause)) =
        queue.target.expect("selected paused target").continued
    else {
        panic!("the selected track waits for its native Stop");
    };
    let incoming = queue
        .active
        .get(queue.incoming_index(first).expect("incoming paused track"))
        .expect("incoming slot");
    assert_eq!(incoming.role, Role::Incoming);
    assert_eq!(incoming.track.snapshot().status, PlayingStatus::Loaded);
    let slot = incoming.slot;
    let mut stopped = rig
        .block(SessionFrame::new(128), 0.0)
        .expect("native Stop block");
    assert_eq!(stopped.len(), 1);
    let receipt = stopped.pop().expect("native Stop receipt");
    assert_eq!(receipt.seq(), pause);
    let (outcome, mut batch) = receipt.into();
    assert!(matches!(outcome, Outcome::Applied { at, .. } if at == SessionFrame::new(128)));
    let stopped_mark = SlotMark {
        session: SessionFrame::new(128),
        lane: LaneFrame {
            segment: kithara_signal::SegmentId::FIRST,
            frame: 0,
        },
        position: Duration::ZERO,
    };
    assert!(matches!(
        batch.commands.as_slice(),
        [DeckPart::Returned(Returned::Stopped { slot: stopped_slot, resume: Some(mark) })]
            if *stopped_slot == slot && *mark == stopped_mark
    ));
    assert_eq!(queue.current, None);
    assert_eq!(
        queue
            .active
            .get(
                queue
                    .incoming_index(first)
                    .expect("unsettled incoming track")
            )
            .expect("unsettled slot")
            .role,
        Role::Incoming
    );
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq: pause,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    assert_eq!(queue.current, Some(first));
    let active = queue
        .active
        .get(queue.active_current_index().expect("current slot"))
        .expect("slot");
    assert_eq!(active.role, Role::Current);
    assert_eq!(
        active.track.snapshot().status,
        PlayingStatus::Paused { at: Duration::ZERO }
    );
    assert!(queue.target.is_none());
    rig.ring.publish().expect("publish paused entry");
    rig.inbox.drain();
    assert!(
        rig.inbox
            .scope(rig.scope)
            .expect("deck scope")
            .next_due(SessionFrame::new(128), 1)
            .is_none()
    );
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Play { at: When::Next }, out)
    })
    .expect("play the current paused track");
    crate::mock::settle(&mut queue, &mut rig, SessionFrame::new(128));
    assert!(matches!(
        queue
            .current_track()
            .expect("current track")
            .snapshot()
            .status,
        PlayingStatus::Playing { .. }
    ));
}

#[kithara::test(tokio)]
async fn play_with_a_pending_selection_enters_that_selection() {
    let (mut queue, first, second, mut rig, dir) = pending_selection();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Play { at: When::Next }, out)
    })
    .expect("play pending selection");
    assert_eq!(queue.target.expect("selected target").to, first);
    answer_load(&mut queue, &mut rig, &dir).await;
    answer_deck(&mut queue, &mut rig, SessionFrame::new(0));
    assert!(matches!(
        queue
            .active
            .get(queue.incoming_index(first).expect("incoming first"))
            .expect("slot")
            .role,
        Role::Incoming
    ));
    assert!(
        queue
            .pending_handover
            .is_some_and(|pending| pending.item == first)
    );
    answer_deck(&mut queue, &mut rig, SessionFrame::new(128));
    assert_eq!(queue.current, Some(first));
    assert_ne!(queue.current, Some(second));
    assert!(queue.target.is_none());
}

#[kithara::test(tokio)]
#[case::stop_first(false)]
#[case::stale_first(true)]
async fn a_pause_over_a_scheduled_entry_settles_current_paused(#[case] reverse: bool) {
    let (mut queue, first, _, mut rig, dir) = pending_selection();
    let scheduled = SessionFrame::new(256);
    queue.target.as_mut().expect("selected target").bound =
        Some(kithara_play::Bound::AtOrAfter(scheduled));
    answer_load(&mut queue, &mut rig, &dir).await;
    assert!(
        queue.pending_handover.is_none(),
        "the future entry waits for native attachment"
    );
    answer_deck(&mut queue, &mut rig, SessionFrame::new(0));
    assert_eq!(queue.current, None);
    assert!(queue.pending_handover.is_some());
    rig.ring.publish().expect("publish future entry");
    rig.inbox.drain();
    assert!(
        rig.inbox
            .scope(rig.scope)
            .expect("deck scope")
            .next_due(SessionFrame::new(0), 1)
            .is_none(),
        "the scheduled entry cannot start on Next"
    );
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Pause { at: When::Next }, out)
    })
    .expect("withdraw the scheduled entry");
    rig.ring.publish().expect("publish Stop");
    rig.inbox.drain();
    {
        let mut level = rig.inbox.scope(rig.scope).expect("deck scope");
        let mut stop = level
            .next_due(SessionFrame::new(0), 1)
            .expect("Stop before Start");
        for part in stop.commands_mut() {
            let DeckPart::Stop { slot, .. } = *part else {
                panic!("withdrawal only stops the incoming slot");
            };
            *part = DeckPart::Returned(Returned::Stopped {
                slot,
                resume: Some(SlotMark {
                    session: SessionFrame::new(0),
                    lane: LaneFrame::default(),
                    position: Duration::ZERO,
                }),
            });
        }
        stop.apply(());
    }
    let mut receipts = std::iter::from_fn(|| rig.ring.receipt()).collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| matches!(
                receipt, ScopedReceipt::Scope(_, receipt)
                    if matches!(receipt.outcome(), Outcome::Rejected(Rejection::Stale))
            ))
            .count(),
        1
    );
    if reverse {
        receipts.reverse();
    }
    for receipt in receipts {
        let ScopedReceipt::Scope(_, receipt) = receipt else {
            panic!("withdrawal must answer on its deck scope");
        };
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::settle(
                queue,
                TrackReceipt::Deck {
                    seq,
                    outcome: &outcome,
                    batch: &mut batch,
                },
                out,
            )
        });
    }
    assert_eq!(queue.current, Some(first));
    assert_eq!(
        queue
            .current_track()
            .expect("current track")
            .snapshot()
            .status,
        PlayingStatus::Paused { at: Duration::ZERO }
    );
    assert!(queue.target.is_none());
    rig.ring.publish().expect("publish settled entry");
    rig.inbox.drain();
    assert!(
        rig.inbox
            .scope(rig.scope)
            .expect("deck scope")
            .next_due(scheduled, 1)
            .is_none()
    );
}
