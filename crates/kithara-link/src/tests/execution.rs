use std::sync::Arc;

use kithara_assets::AssetStore;
use kithara_audio::AudioObserverSlot;
use kithara_command::{Batch, Inbox, Outcome, Receipt, Rejection, Seq, When};
use kithara_host::HostOwner;
use kithara_play::{
    Bound, DeckPass, Outbox, PlayWorker, PlayWorkerConfig, Player, PlayerConfig, PlayerImpl,
    ResourceConfig, ResourceLoad, ResourceSrc, Track, TrackCommand, TrackReceipt, TrackSettings,
    TrackStatus,
    mock::{self, DeckRig},
};
use kithara_render::{
    Dispatched, DispatcherCommand, LaneApplied, LaneCommand, LaneFrame, LaneProtocol,
    bridge::{DeckMixSettings, DeckProtocol, DeckSnapshot, Slot, SlotMark},
    rt::DeckMixerConfig,
};
use kithara_signal::{AudioChunkInfo, AudioSpec, FrameCount, SegmentId, SessionFrame};
use kithara_sync::SpeedCurve;
use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
    kithara,
};

use super::{
    fixtures::{answer, grid, load, loaded, position, trajectory},
    host_fixture::{host, register},
};
use crate::{
    GridAnswer, LinkConfig, Linked, LinkedConfig, LinkedHostCommand, LinkedPlayer, SyncStatus,
};

fn settle(
    deck: &mut Linked<PlayerImpl<TestPools>>,
    rig: &mut DeckRig<TestPools>,
    receipt: Receipt<DeckProtocol>,
) {
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    rig.with_outbox(|out| {
        deck.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    })
    .expect("live deck scope");
}

// Ruling: spec 3/5.5 removes staged sync lanes; supersession cancels queued starts by Seq on the one resident player.
#[kithara::test(tokio)]
async fn a_lane_dropped_before_its_turn_is_reported_only_by_its_cancellation() {
    let directory = TestTempDir::new();
    let path = directory.path().join("track.wav");
    let samples = vec![0.25; usize::try_from(mock::SAMPLE_RATE.get()).expect("sample rate") * 2];
    mock::write_pcm_wav(&path, &samples, AudioSpec::new(2, mock::SAMPLE_RATE)).expect("local WAV");
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(ResourceSrc::Path(path))
        .store(AssetStore::builder(pools()).build())
        .worker(worker.clone())
        .host_sample_rate(mock::SAMPLE_RATE)
        .build();
    let (_, source) = mock::resource_tracks(&config).expect("local source");
    let futures::future::Either::Left(source) = source else {
        panic!("WAV is a local source")
    };
    let fixture = mock::track_load(
        source,
        Arc::from("resident"),
        worker.clone(),
        mock::SAMPLE_RATE,
        |worker, config, position, start, inbox| {
            Box::pin(async move { worker.load(config, position, start, inbox).await })
        },
    );
    let track: PlayerImpl<TestPools> = PlayerImpl::new(PlayerConfig {
        item: kithara_events::TrackId::allocate(),
        slot: Some(Slot::new(0)),
        settings: TrackSettings::default(),
    })
    .expect("track");
    let mut deck = Linked::new(
        LinkedConfig::builder()
            .inner(track)
            .settings(LinkConfig::default())
            .trajectory(trajectory(120.0, 4))
            .build(),
    );
    let mut rig = DeckRig::new(DeckMixerConfig::default()).expect("deck scope");
    rig.with_outbox(|out| {
        deck.apply(
            TrackCommand::Load {
                item: ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay())),
                position: position(0),
            },
            out,
        )
    })
    .expect("scope")
    .expect("one load");
    let opened = rig
        .load_fixture(&worker, fixture, position(0))
        .await
        .expect("resident lane");
    let receipt = rig.open(Ok(opened)).expect("one dispatcher receipt");
    rig.with_outbox(|out| deck.settle(TrackReceipt::Loaded(receipt), out))
        .expect("scope");
    for receipt in rig
        .block(SessionFrame::new(0), 0.0)
        .expect("attachment block")
    {
        settle(&mut deck, &mut rig, receipt);
    }
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    let first = rig
        .with_outbox(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(24_000)),
                },
                out,
            )
        })
        .expect("scope")
        .expect("first entry")
        .expect("first sequence");
    let first_receipts = rig
        .block(SessionFrame::new(24_000), 0.0)
        .expect("first turn");
    assert_eq!(first_receipts.len(), 1);
    assert_eq!(first_receipts[0].seq(), first);
    assert!(
        matches!(first_receipts[0].outcome(), Outcome::Applied { at, .. } if *at == SessionFrame::new(24_000))
    );
    for receipt in first_receipts {
        settle(&mut deck, &mut rig, receipt);
    }
    let mut queued = Vec::new();
    for frame in [48_000, 72_000] {
        queued.push(
            rig.with_outbox(|out| {
                deck.apply(
                    TrackCommand::Play {
                        at: When::At(SessionFrame::new(frame)),
                    },
                    out,
                )
            })
            .expect("scope")
            .expect("queued entry")
            .expect("queued sequence"),
        );
    }
    rig.with_outbox(|out| deck.apply(TrackCommand::Supersede, out))
        .expect("scope")
        .expect("cancel queued entries");
    rig.with_outbox(|out| deck.apply(TrackCommand::Release, out))
        .expect("scope")
        .expect("release resident lane");
    let receipts = rig
        .block(SessionFrame::new(24_001), 0.0)
        .expect("cancellation turn");
    let cancelled: Vec<_> = receipts
        .iter()
        .filter(|receipt| queued.contains(&receipt.seq()))
        .collect();
    assert_eq!(cancelled.len(), 2);
    assert_eq!(
        cancelled
            .iter()
            .map(|receipt| receipt.seq())
            .collect::<Vec<_>>(),
        queued
    );
    assert!(
        cancelled
            .iter()
            .all(|receipt| matches!(receipt.outcome(), Outcome::Rejected(Rejection::Stale)))
    );
    for receipt in receipts {
        settle(&mut deck, &mut rig, receipt);
    }
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Released);
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(
        rig.block(SessionFrame::new(72_000), 0.0)
            .expect("obsolete turn")
            .is_empty()
    );
}

// Ruling: spec 4.5/4.6 replaces attached group topology with one item/load-owned track grid, never a nested group.
#[kithara::test]
fn an_attached_group_owns_its_track_geometry_as_its_only_member() {
    let (mut deck, control, mut rig, loading) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    let item = deck.snapshot().as_ref().item;
    let slot = deck.snapshot().as_ref().slot;
    let sync = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    assert_eq!(control.commands().len(), 2);
    assert_eq!(control.commands()[0].0, sync);
    assert_eq!(control.commands()[1].0, sync);
    rig.apply(&mut deck, sync, 0);
    assert_eq!(deck.snapshot().as_ref().item, item);
    assert_eq!(deck.snapshot().as_ref().slot, slot);
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(24_000, 12_000, Some((4, 0)), 960_000),
    );
    assert_eq!(deck.snapshot().as_ref().item, item);
    assert_eq!(deck.snapshot().as_ref().slot, slot);
    assert!(
        !control
            .commands()
            .iter()
            .any(|(_, command)| matches!(command, super::fixtures::Command::Load(_)))
    );
    let replacement = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, replacement, 0);
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(24_000, 0, Some((4, 0)), 960_000),
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(0)
        }
    );
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.6 removes recursive groups; a receipt routed by DeckId updates only the registered issuing player.
#[kithara::test]
fn a_receipt_reaches_the_nested_group_that_issued_it() {
    let (mut host, clock) = host();
    let (deck, control, _, _) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    let (id, probe) = register(&mut host, deck, &control);
    let prepared = host
        .apply(LinkedHostCommand::Sync { deck: id, on: true })
        .expect("sync")
        .expect("prepared segment");
    let outcome = Outcome::Applied {
        at: SessionFrame::new(0),
        data: (),
    };
    let mut batch = Batch {
        basis: vec![(Slot::new(0), None)],
        commands: Vec::new(),
    };
    host.with_deck(id, &mut |deck, out, pass| {
        kithara_play::HostedDeck::settle(
            deck,
            TrackReceipt::Deck {
                seq: prepared,
                outcome: &outcome,
                batch: &mut batch,
            },
            pass,
            out,
        );
    })
    .expect("prepared segment receipt");
    control.clear();
    clock.at(24_000);
    host.with_deck(id, &mut |deck, out, pass| {
        kithara_play::HostedDeck::tick(deck, pass, out);
    })
    .expect("registration pass");
    let (sibling, sibling_control, _, _) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    let (_, sibling_probe) = register(&mut host, sibling, &sibling_control);
    let sibling_before = sibling_probe.read(|observation| observation.snapshot.clone());
    probe.send(TrackCommand::Play {
        at: When::At(SessionFrame::new(96_000)),
    });
    host.with_deck(id, &mut |deck, out, pass| {
        kithara_play::HostedDeck::drain(deck, pass, out);
    })
    .expect("issuing deck");
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    let seq = commands[0].0;
    assert_eq!(
        commands[0].1,
        super::fixtures::Command::Play(When::At(SessionFrame::new(96_000)))
    );
    let outcome = Outcome::Applied {
        at: SessionFrame::new(96_000),
        data: (),
    };
    let mut batch = Batch {
        basis: vec![(Slot::new(0), None)],
        commands: Vec::new(),
    };
    host.with_deck(id, &mut |deck, out, pass| {
        kithara_play::HostedDeck::settle(
            deck,
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            pass,
            out,
        );
    })
    .expect("routed receipt");
    probe.read(|observation| {
        assert_eq!(observation.snapshot.sync, SyncStatus::On);
        assert_eq!(
            observation.snapshot.as_ref().status,
            TrackStatus::Playing {
                since: SessionFrame::new(96_000)
            }
        );
    });
    sibling_probe.read(|observation| {
        assert_eq!(observation.snapshot.sync, SyncStatus::Off);
        assert_eq!(
            observation.snapshot.as_ref().item,
            sibling_before.as_ref().item
        );
        assert_eq!(
            observation.snapshot.as_ref().status,
            sibling_before.as_ref().status
        );
        assert_eq!(
            observation.snapshot.as_ref().position,
            sibling_before.as_ref().position
        );
    });
    assert!(sibling_control.commands().is_empty());
}

async fn native_linked(
    directory: &TestTempDir,
) -> (
    Linked<PlayerImpl<TestPools>>,
    DeckRig<TestPools>,
    Inbox<LaneProtocol>,
    Seq,
) {
    let path = directory.path().join("paused.wav");
    let samples = vec![0.25; usize::try_from(mock::SAMPLE_RATE.get()).expect("rate") * 2];
    mock::write_pcm_wav(&path, &samples, AudioSpec::new(2, mock::SAMPLE_RATE)).expect("WAV");
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(ResourceSrc::Path(path))
        .store(AssetStore::builder(pools()).build())
        .worker(worker.clone())
        .host_sample_rate(mock::SAMPLE_RATE)
        .build();
    let track: PlayerImpl<TestPools> = PlayerImpl::new(PlayerConfig {
        item: kithara_events::TrackId::allocate(),
        slot: Some(Slot::new(0)),
        settings: TrackSettings::default(),
    })
    .expect("track");
    let mut deck = Linked::new(
        LinkedConfig::builder()
            .inner(track)
            .settings(LinkConfig::default())
            .trajectory(trajectory(120.0, 4))
            .build(),
    );
    let mut rig = DeckRig::new(DeckMixerConfig::default()).expect("scope");
    let loading = native_marked(&mut rig, SegmentId::FIRST, |out| {
        deck.apply(
            TrackCommand::Load {
                item: ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay())),
                position: position(0),
            },
            out,
        )
    })
    .expect("load")
    .expect("load sequence");
    rig.opens.drain();
    let mut due = rig.opens.next_due((), 1).expect("owner load");
    assert_eq!(due.seq(), loading);
    let DispatcherCommand::Load(request) = due.commands_mut().pop().expect("load command") else {
        panic!("one load");
    };
    assert!(due.commands().is_empty());
    due.defer();
    let inbox = request.inbox;
    let opened = rig
        .load_fixture(&worker, request.item, request.position)
        .await
        .expect("PCM fixture");
    assert_eq!(
        opened
            .opened
            .pcm
            .next_chunk_info()
            .expect("the dispatcher prepared actual source PCM")
            .media_end,
        Some(position(48_000)),
        "the source packet must declare the one-second fixture endpoint"
    );
    rig.opens
        .resume(loading, (), ())
        .expect("owner load resumes")
        .apply(Dispatched::Loaded(opened));
    let receipt = rig
        .dispatcher
        .receipts()
        .next()
        .expect("owner load receipt");
    assert_eq!(receipt.seq(), loading);
    native_marked(&mut rig, SegmentId::FIRST, |out| {
        deck.settle(TrackReceipt::Loaded(receipt), out)
    });
    for receipt in rig.block(SessionFrame::new(0), 0.0).expect("attach") {
        native_settle(&mut deck, &mut rig, SegmentId::FIRST, receipt);
    }
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    let duration = deck
        .snapshot()
        .as_ref()
        .duration
        .expect("decoded WAV duration");
    assert_eq!(duration, position(48_000));
    let mut raw = grid(24_000, 0, None, 48_000).as_raw().clone();
    raw.duration = Some(duration.as_secs_f64());
    let model = kithara_beat::BeatGridModel::try_from(raw).expect("analysis of the decoded WAV");
    assert_eq!(model.as_raw().duration, Some(duration.as_secs_f64()));
    assert_eq!(
        model.as_raw().beats.last().expect("EOF beat").at,
        duration.as_secs_f64()
    );
    let item = deck.snapshot().as_ref().item;
    native_marked(&mut rig, SegmentId::FIRST, |out| {
        LinkedPlayer::grid(
            &mut deck,
            GridAnswer {
                item,
                load: loading,
                model: Ok(model),
            },
            out,
        );
    });
    (deck, rig, inbox, loading)
}

fn native_ready(inbox: &mut Inbox<LaneProtocol>, clock: SegmentId) -> SegmentId {
    inbox.drain();
    let due = inbox
        .next_due(
            LaneFrame {
                segment: clock,
                frame: 0,
            },
            1,
        )
        .expect("native Segment");
    let [LaneCommand::Segment { id, .. }] = due.commands() else {
        panic!("one native Segment");
    };
    let segment = *id;
    due.apply(LaneApplied {
        source_basis: None,
        engine_latency: FrameCount::new(0),
        ready: Some(segment),
    });
    segment
}

fn native_marked<R>(
    rig: &mut DeckRig<TestPools>,
    segment: SegmentId,
    run: impl FnOnce(&mut Outbox<'_, TestPools>) -> R,
) -> R {
    let output = mock::output(None).get();
    let mut snapshot = DeckSnapshot::new(DeckMixerConfig::default());
    snapshot.slots[0].mark = Some(SlotMark {
        session: SessionFrame::new(0),
        lane: LaneFrame { segment, frame: 0 },
        position: position(0),
    });
    snapshot.slots[0].source = Some(AudioChunkInfo {
        spec: AudioSpec::new(2, mock::SAMPLE_RATE),
        segment,
        media_origin: Some(position(0)),
        visible_origin: Some(position(0)),
        media_end: Some(position(48_000)),
        ..AudioChunkInfo::default()
    });
    let pass = DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: SessionFrame::new(0),
        delivery: FrameCount::new(0),
        output: &output,
        deck: &snapshot,
    };
    let mut scope = rig.ring.scope(rig.scope).expect("scope");
    run(&mut Outbox::new(&mut scope, &mut rig.dispatcher).in_pass(pass))
}

fn native_settle(
    deck: &mut Linked<PlayerImpl<TestPools>>,
    rig: &mut DeckRig<TestPools>,
    current: SegmentId,
    receipt: Receipt<DeckProtocol>,
) {
    let applied = matches!(receipt.outcome(), Outcome::Applied { .. });
    let segment = receipt
        .batch()
        .commands
        .iter()
        .fold(current, |segment, command| match command {
            kithara_render::bridge::DeckPart::Adopt { segment, .. } if applied => *segment,
            _ => segment,
        });
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    native_marked(rig, segment, |out| {
        deck.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
}

async fn paused_future_speed(
    directory: &TestTempDir,
) -> (
    Linked<PlayerImpl<TestPools>>,
    DeckRig<TestPools>,
    Inbox<LaneProtocol>,
    SegmentId,
) {
    let (mut deck, mut rig, mut inbox, _) = native_linked(directory).await;
    native_marked(&mut rig, SegmentId::FIRST, |out| {
        LinkedPlayer::sync(&mut deck, true, out)
    })
    .expect("sync")
    .expect("cue");
    let segment = native_ready(&mut inbox, SegmentId::FIRST);
    for receipt in rig.block(SessionFrame::new(0), 0.0).expect("adopt") {
        native_settle(&mut deck, &mut rig, segment, receipt);
    }
    native_marked(&mut rig, segment, |out| {
        deck.tick(SessionFrame::new(0), out);
    });
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
    native_marked(&mut rig, segment, |out| {
        deck.apply(TrackCommand::Play { at: When::Next }, out)
    })
    .expect("play")
    .expect("start sequence");
    for receipt in rig.block(SessionFrame::new(0), 0.0).expect("start") {
        native_settle(&mut deck, &mut rig, segment, receipt);
    }
    assert!(matches!(
        deck.snapshot().as_ref().status,
        TrackStatus::Playing { .. }
    ));
    let speed = native_marked(&mut rig, segment, |out| {
        deck.apply(
            TrackCommand::SetSpeed {
                speed: SpeedCurve::Approach(
                    kithara_signal::ExponentialApproach::new(1.0, 0.9, 220.5).expect("law"),
                ),
                at: When::At(SessionFrame::new(100_000)),
            },
            out,
        )
    })
    .expect("future speed")
    .expect("future sequence");
    assert_eq!(deck.speed_applied(speed), None);
    native_marked(&mut rig, segment, |out| {
        deck.apply(TrackCommand::Pause { at: When::Next }, out)
    })
    .expect("pause")
    .expect("stop sequence");
    for receipt in rig.block(SessionFrame::new(0), 0.0).expect("stop") {
        native_settle(&mut deck, &mut rig, segment, receipt);
    }
    assert_eq!(
        deck.snapshot().as_ref().status,
        TrackStatus::Paused { at: position(0) }
    );
    assert!(deck.snapshot().as_ref().pending_lane);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
    (deck, rig, inbox, segment)
}

#[kithara::test(tokio)]
#[case::direct_play(false)]
#[case::owner_preparation(true)]
async fn a_ready_paused_native_segment_can_rebind_while_a_future_speed_is_pending(
    #[case] prepare: bool,
) {
    let directory = TestTempDir::new();
    let (mut deck, mut rig, mut inbox, current) = paused_future_speed(&directory).await;
    if prepare {
        assert!(
            !native_marked(&mut rig, current, |out| deck
                .prepare_entry(SessionFrame::new(0), out))
            .expect("entry preparation")
        );
    } else {
        assert_eq!(
            native_marked(&mut rig, current, |out| deck
                .apply(TrackCommand::Play { at: When::Next }, out))
            .expect("play preparation"),
            None
        );
    }
    let replacement = native_ready(&mut inbox, current);
    assert_ne!(
        replacement, current,
        "the future speed cannot block entry rebinding"
    );
    for receipt in rig.block(SessionFrame::new(0), 0.0).expect("adopt") {
        native_settle(&mut deck, &mut rig, replacement, receipt);
    }
    if prepare {
        assert!(
            native_marked(&mut rig, replacement, |out| deck
                .prepare_entry(SessionFrame::new(0), out))
            .expect("prepared entry")
        );
        assert!(
            rig.block(SessionFrame::new(0), 0.0)
                .expect("no owner start")
                .is_empty()
        );
        assert_eq!(
            deck.snapshot().as_ref().status,
            TrackStatus::Paused { at: position(0) }
        );
    } else {
        native_marked(&mut rig, replacement, |out| {
            deck.tick(SessionFrame::new(0), out);
        });
        let receipts = rig
            .block(SessionFrame::new(0), 0.0)
            .expect("prepared start");
        assert_eq!(
            receipts.len(),
            1,
            "waiting Play resumes despite unrelated future work"
        );
        for receipt in receipts {
            native_settle(&mut deck, &mut rig, replacement, receipt);
        }
        assert_eq!(
            deck.snapshot().as_ref().status,
            TrackStatus::Playing {
                since: SessionFrame::new(0)
            }
        );
    }
}

#[kithara::test(tokio)]
async fn an_old_native_ready_cannot_resume_the_waiting_linked_replacement() {
    let directory = TestTempDir::new();
    let (mut deck, mut rig, mut inbox, current) = paused_future_speed(&directory).await;
    native_marked(&mut rig, current, |out| {
        deck.cue(position(0), SpeedCurve::Constant(1.0), out)
    })
    .expect("old cue");
    native_marked(&mut rig, current, |out| {
        deck.cue(position(0), SpeedCurve::Constant(1.0), out)
    })
    .expect("replacement cue");
    assert_eq!(
        native_marked(&mut rig, current, |out| deck
            .apply(TrackCommand::Play { at: When::Next }, out))
        .expect("waiting Play"),
        None
    );
    let old = native_ready(&mut inbox, current);
    native_marked(&mut rig, current, |out| {
        deck.tick(SessionFrame::new(0), out);
    });
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
    for receipt in rig.block(SessionFrame::new(0), 0.0).expect("cue adoptions") {
        assert!(
            !receipt
                .batch()
                .commands
                .iter()
                .any(|command| matches!(command, kithara_render::bridge::DeckPart::Start { .. }))
        );
        native_settle(&mut deck, &mut rig, current, receipt);
    }
    let replacement = native_ready(&mut inbox, current);
    assert_ne!(replacement, old);
    native_marked(&mut rig, replacement, |out| {
        deck.tick(SessionFrame::new(0), out);
    });
    let receipts = rig.block(SessionFrame::new(0), 0.0).expect("current start");
    assert_eq!(
        receipts.len(),
        1,
        "only the current native Ready resumes Play"
    );
    for receipt in receipts {
        native_settle(&mut deck, &mut rig, replacement, receipt);
    }
    assert_eq!(
        deck.snapshot().as_ref().status,
        TrackStatus::Playing {
            since: SessionFrame::new(0)
        }
    );
}
