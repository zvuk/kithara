pub(super) use kithara_assets::{AssetStore, StorageBackend};
pub(super) use kithara_audio::AudioObserverSlot;
pub(super) use kithara_command::{Outcome, Rejection, ScopedReceipt, Seq, When};
pub(super) use kithara_events::TrackId;
pub(super) use kithara_platform::time::Duration;
pub(super) use kithara_play::{
    DeckMixSettings, DeckMixerConfig, DeckPart, DeckPass, Outbox, PlayWorker, PlayWorkerConfig,
    Player, ResourceConfig, ResourceLoad, ResourcePrep, ResourceSrc, TrackReceipt,
    TrackStatus as PlayingStatus, mock,
};
pub(super) use kithara_render::{
    LaneFrame,
    bridge::{Returned, SlotMark},
};
pub(super) use kithara_signal::{AudioSpec, FrameCount, SessionFrame};
pub(super) use kithara_test_utils::{TestTempDir, kithara};

use super::*;
pub(super) use crate::{
    QueueConfig, TrackSource,
    test_pools::{TestPools, pools},
};

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

pub(super) fn with_outbox<Value>(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    run: impl FnOnce(&mut Queue<TestPools>, &mut Outbox<'_, TestPools>) -> Value,
) -> Value {
    let output = mock::output(None).get();
    let deck = queue.deck.mixer.clone();
    let pass = DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: SessionFrame::new(0),
        delivery: FrameCount::new(128),
        output: &output,
        deck: &deck,
    };
    let mut scope = rig.ring.scope(rig.scope).expect("deck scope");
    let mut out = Outbox::new(&mut scope, &mut rig.dispatcher).in_pass(pass);
    run(queue, &mut out)
}

pub(super) fn pending_selection() -> (
    Queue<TestPools>,
    TrackId,
    TrackId,
    mock::DeckRig<TestPools>,
    TestTempDir,
) {
    let (mut queue, mut rig, dir) = empty_queue();
    let path = dir.path().join("entry.wav");
    let first = TrackId::allocate();
    let second = TrackId::allocate();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        for id in [first, second] {
            Player::apply(
                queue,
                QueueCommand::Append {
                    id,
                    source: TrackSource::Uri(path.to_str().expect("fixture path").to_owned()),
                },
                out,
            )
            .expect("append fixture");
        }
        Player::apply(
            queue,
            QueueCommand::Select {
                id: first,
                transition: super::super::super::Transition::None,
            },
            out,
        )
        .expect("select first");
    });
    (queue, first, second, rig, dir)
}

pub(super) async fn loaded_fixture(
    rig: &mut mock::DeckRig<TestPools>,
    dir: &TestTempDir,
) -> kithara_render::Loaded<kithara_play::OpenedTrack> {
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let config = ResourceConfig::for_src(ResourceSrc::Path(dir.path().join("entry.wav")))
        .store(
            AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build();
    let prepared = prep
        .prepare(config, &mock::output(None).get())
        .expect("prepared fixture");
    let item = ResourceLoad::new(prepared, Box::new(AudioObserverSlot::default().relay()));
    let mut loaded = rig
        .load_fixture(item, Duration::ZERO)
        .await
        .expect("opened fixture");
    loaded.opened.metadata.title = Some("Loaded fixture".to_owned());
    loaded
}

pub(super) async fn answer_load(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    dir: &TestTempDir,
) {
    let loaded = loaded_fixture(rig, dir).await;
    answer_loaded(queue, rig, loaded);
}

pub(super) fn answer_loaded(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    loaded: kithara_render::Loaded<kithara_play::OpenedTrack>,
) {
    use kithara_render::{Dispatched, DispatcherCommand};

    let mut loaded = Some(loaded);
    rig.opens.drain();
    loop {
        let mut due = rig.opens.next_due((), 1).expect("dispatcher command");
        let opens = matches!(due.commands_mut().as_slice(), [DispatcherCommand::Load(_)]);
        match due.commands_mut().as_slice() {
            [DispatcherCommand::Load(_)] => {
                due.apply(Dispatched::Loaded(loaded.take().expect("load fixture")));
            }
            [DispatcherCommand::Release(_)] => due.apply(Dispatched::Released),
            [DispatcherCommand::SetPriority(_, _)] => due.apply(Dispatched::Prioritized),
            _ => panic!("one dispatcher command per batch"),
        }
        let receipt = rig
            .dispatcher
            .receipts()
            .next()
            .expect("dispatcher receipt");
        with_outbox(queue, rig, |queue, out| {
            Player::settle(queue, TrackReceipt::Loaded(receipt), out)
        });
        if opens {
            break;
        }
    }
}

pub(super) fn answer_deck(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    at: SessionFrame,
) -> Seq {
    rig.ring.publish().expect("publish deck batch");
    rig.inbox.drain();
    rig.inbox
        .scope(rig.scope)
        .expect("deck scope")
        .next_due(at, 1)
        .expect("pending deck batch")
        .apply(());
    let ScopedReceipt::Scope(_, receipt) = rig.ring.receipt().expect("deck receipt") else {
        panic!("entry must answer on its deck scope");
    };
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    assert!(
        batch
            .commands
            .iter()
            .any(|part| matches!(part, DeckPart::Attach { .. } | DeckPart::Start { .. }))
    );
    with_outbox(queue, rig, |queue, out| {
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
    seq
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
    answer_deck(&mut queue, &mut rig, SessionFrame::new(0));
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
    answer_deck(&mut queue, &mut rig, SessionFrame::new(128));
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
        Role::Incoming { batch: Some(_) }
    ));
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
    answer_load(&mut queue, &mut rig, &dir).await;
    answer_deck(&mut queue, &mut rig, SessionFrame::new(0));
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
                resume: SlotMark {
                    session: SessionFrame::new(0),
                    lane: LaneFrame::default(),
                    position: Duration::ZERO,
                },
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
            .next_due(SessionFrame::new(128), 1)
            .is_none()
    );
}

pub(super) fn empty_queue() -> (Queue<TestPools>, mock::DeckRig<TestPools>, TestTempDir) {
    queue_with_load_settings(std::num::NonZeroUsize::new(3).expect("default cap"), false)
}

pub(super) fn queue_with_load_settings(
    max_concurrent_loads: std::num::NonZeroUsize,
    should_autoplay: bool,
) -> (Queue<TestPools>, mock::DeckRig<TestPools>, TestTempDir) {
    let dir = TestTempDir::new();
    let path = dir.path().join("entry.wav");
    mock::write_pcm_wav(&path, &[0.5; 2_048], AudioSpec::new(2, mock::SAMPLE_RATE))
        .expect("float WAV fixture");
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let store = AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .build();
    let mut queue = Queue::new(
        QueueConfig::builder()
            .prep(prep)
            .store(store)
            .max_concurrent_loads(max_concurrent_loads)
            .should_autoplay(should_autoplay)
            .build(),
    );
    queue.clock = Some((SessionFrame::new(0), FrameCount::new(128)));
    queue.deck.mixer.sample_rate = mock::SAMPLE_RATE.get();
    let rig = mock::DeckRig::new(DeckMixerConfig::default()).expect("fixture command ring");
    (queue, rig, dir)
}

pub(super) fn tick_fixture(queue: &mut Queue<TestPools>, rig: &mut mock::DeckRig<TestPools>) {
    let events = rig.events.drain().collect::<Vec<_>>();
    let output = mock::output(None).get();
    let deck = queue.deck.mixer.clone();
    let pass = DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: SessionFrame::new(0),
        delivery: FrameCount::new(128),
        output: &output,
        deck: &deck,
    };
    let mut scope = rig.ring.scope(rig.scope).expect("deck scope");
    let mut out = Outbox::new(&mut scope, &mut rig.dispatcher).in_pass(pass);
    for event in events {
        HostedDeck::settle(queue, TrackReceipt::Event(event), pass, &mut out);
    }
    HostedDeck::tick(queue, pass, &mut out);
}
