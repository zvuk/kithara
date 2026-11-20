use std::num::NonZeroUsize;

use kithara_command::{Batch, ChannelConfig, channel};
use kithara_decode::DecodeError;
use kithara_events::EventReceiver;
use kithara_play::{LoadRefusal, Settled, Track, TrackSettingsChange};
use kithara_render::{Dispatched, DispatcherCommand, Loaded, ServiceClass};

use super::*;
use crate::{
    Transition,
    mock::{fixture_uri, load_seq},
    queue::slots::{LoadState, Slots},
};

fn append(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    dir: &TestTempDir,
    count: usize,
) -> Vec<TrackId> {
    (0..count)
        .map(|_| {
            let id = TrackId::allocate();
            with_outbox(queue, rig, |queue, out| {
                Player::apply(
                    queue,
                    QueueCommand::Append {
                        position: kithara_play::Position::ZERO,
                        id,
                        source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
                    },
                    out,
                )
            })
            .expect("append fixture");
            id
        })
        .collect()
}

fn select(queue: &mut Queue<TestPools>, rig: &mut mock::DeckRig<TestPools>, id: TrackId) {
    with_outbox(queue, rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Select {
                id,
                transition: Transition::None,
            },
            out,
        )
    })
    .expect("select fixture");
}

fn settle_dispatcher(queue: &mut Queue<TestPools>, rig: &mut mock::DeckRig<TestPools>) {
    let receipts = rig.dispatcher.receipts().collect::<Vec<_>>();
    for receipt in receipts {
        with_outbox(queue, rig, |queue, out| {
            Player::settle(queue, TrackReceipt::Loaded(receipt), out)
        });
    }
}

#[kithara::hang_watchdog]
fn hold_loads(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
) -> (Vec<(Seq, ServiceClass)>, Vec<ServiceClass>) {
    let mut loads = Vec::new();
    let mut priorities = Vec::new();
    rig.opens.drain();
    while let Some(due) = rig.opens.next_due((), 1) {
        hang_tick!();
        match due.commands() {
            [DispatcherCommand::Load(request)] => {
                let class = request.class;
                loads.push((due.defer(), class));
            }
            [DispatcherCommand::Release(_)] => due.apply(Dispatched::Released),
            [DispatcherCommand::SetPriority(_, class)] => {
                priorities.push(*class);
                due.apply(Dispatched::Prioritized);
            }
            _ => panic!("one dispatcher operation per batch"),
        }
        hang_reset!();
    }
    settle_dispatcher(queue, rig);
    (loads, priorities)
}

fn answer(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    seq: Seq,
    loaded: Loaded<kithara_play::OpenedTrack>,
) {
    rig.opens
        .resume(seq, (), ())
        .expect("held open")
        .apply(Dispatched::Loaded(loaded));
    settle_dispatcher(queue, rig);
}

#[kithara::hang_watchdog]
fn events(receiver: &mut EventReceiver<QueueEvent>) -> Vec<QueueEvent> {
    std::iter::from_fn(|| {
        hang_tick!();
        receiver.try_recv().ok().map(|envelope| {
            hang_reset!();
            envelope.event
        })
    })
    .collect()
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(ctx = String)]
async fn prefetch_lane_caps_concurrent_loads(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(&worker_owner.0, NonZeroUsize::new(2).expect("cap"), false);
    let mut unanswered = Vec::new();
    let mut max_seen = 0;
    for _ in 0..6 {
        append(&mut queue, &mut rig, &dir, 1);
        unanswered.extend(
            hold_loads(&mut queue, &mut rig)
                .0
                .into_iter()
                .filter(|(_, class)| *class == ServiceClass::Idle)
                .map(|(seq, _)| seq),
        );
        max_seen = max_seen.max(unanswered.len());
    }
    while let Some(seq) = unanswered.pop() {
        let loaded = {
            // WHY: Arm the outer budget only after pacing the real fixture open.
            let _real_io = kithara_platform::flash::real_io();
            hang_tick!(format!("answering held background load {seq:?}"));
            kithara_platform::time::timeout(
                __hang_detector.remaining(),
                loaded_fixture(
                    queue.worker().expect("fixture worker"),
                    &mut rig,
                    &dir,
                    seq,
                    FixtureSource::Synthetic,
                ),
            )
            .await
            .unwrap_or_else(|error| {
                hang_tick!(format!("answering held background load {seq:?}"));
                panic!("held background load did not progress: {error}");
            })
        };
        hang_reset!();
        answer(&mut queue, &mut rig, seq, loaded);
        unanswered.extend(
            hold_loads(&mut queue, &mut rig)
                .0
                .into_iter()
                .filter(|(_, class)| *class == ServiceClass::Idle)
                .map(|(seq, _)| seq),
        );
        max_seen = max_seen.max(unanswered.len());
    }
    assert!(max_seen <= 2, "concurrency exceeded cap: {}", max_seen);
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn spawn_load_bad_url_emits_failed_status(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    append(&mut queue, &mut rig, &dir, 1);
    let id = TrackId::allocate();
    let mut receiver = queue.subscribe::<QueueEvent>();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                position: kithara_play::Position::ZERO,
                id,
                source: TrackSource::Uri("not-a-url".into()),
            },
            out,
        )
    })
    .expect("append does not fail on an invalid source");
    assert_eq!(
        hold_loads(&mut queue, &mut rig).0.len(),
        1,
        "only the initial track opens"
    );
    assert!(matches!(
        queue.track(id).expect("record").status,
        TrackStatus::Failed(_)
    ));
    let mut saw_failed = false;
    for event in events(&mut receiver) {
        if let QueueEvent::TrackStatusChanged {
            id: named, status, ..
        } = event
            && named == id
        {
            match status {
                TrackStatus::Loading => panic!("invalid config must not emit Loading"),
                TrackStatus::Failed(_) => saw_failed = true,
                _ => {}
            }
        }
    }
    assert!(saw_failed, "Failed status event missing");
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn an_appended_track_loads_without_a_select(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[1]);
    let mut receiver = queue.subscribe::<QueueEvent>();
    let loads = hold_loads(&mut queue, &mut rig).0;
    assert!(loads.contains(&(seq, ServiceClass::Idle)));
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    answer(&mut queue, &mut rig, seq, loaded);
    assert_eq!(
        queue.track(ids[1]).expect("record").status,
        TrackStatus::Loaded
    );
    assert!(
        events(&mut receiver)
            .iter()
            .any(|event| matches!(event, QueueEvent::NextTrackReady { id, .. } if *id == ids[1]))
    );
    let parked = queue
        .active
        .parked_iter()
        .find(|parked| parked.item == ids[1])
        .expect("parked track");
    assert_eq!(parked.track.snapshot().slot, None);
    assert!(!parked.track.snapshot().attached);
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn initial_append_loads_at_its_cue_without_a_seek(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let id = TrackId::allocate();
    let position = Duration::from_millis(5);
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                id,
                source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
                position,
            },
            out,
        )
    })
    .expect("append cued initial track");
    rig.opens.drain();
    let mut due = rig.opens.next_due((), 1).expect("initial load");
    let [DispatcherCommand::Load(request)] = due.commands_mut().as_mut_slice() else {
        panic!("one initial load command");
    };
    assert_eq!(request.position, position);
    assert_eq!(request.class, ServiceClass::Warm);
    request.inbox.drain();
    assert!(
        request
            .inbox
            .next_due(
                LaneFrame {
                    segment: kithara_signal::SegmentId::FIRST,
                    frame: 0
                },
                1
            )
            .is_none(),
        "the cue belongs to Load, without a lane seek"
    );
    due.defer();
    tick_fixture(&mut queue, &mut rig);
    assert!(
        hold_loads(&mut queue, &mut rig).0.is_empty(),
        "the cue opens once"
    );
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn preloaded_append_keeps_its_cue_without_seeking_the_current_track(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let first = append(&mut queue, &mut rig, &dir, 1)[0];
    answer_load(&mut queue, &mut rig, FixtureSource::Synthetic).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert!(hold_loads(&mut queue, &mut rig).0.is_empty());
    let outgoing = queue.current_track().expect("current track").snapshot();
    let id = TrackId::allocate();
    let position = Duration::from_millis(5);
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                id,
                source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
                position,
            },
            out,
        )
    })
    .expect("append cued background track");
    rig.opens.drain();
    let mut due = rig.opens.next_due((), 1).expect("background load");
    let [DispatcherCommand::Load(request)] = due.commands_mut().as_mut_slice() else {
        panic!("one background load command");
    };
    assert_eq!(request.position, position);
    assert_eq!(request.class, ServiceClass::Idle);
    let seq = due.defer();
    select(&mut queue, &mut rig, id);
    assert!(
        hold_loads(&mut queue, &mut rig).0.is_empty(),
        "selection reuses the cued load"
    );
    let mut due = rig
        .opens
        .resume(seq, (), ())
        .expect("retained background load");
    let [DispatcherCommand::Load(request)] = due.commands_mut().as_mut_slice() else {
        panic!("the original cued load is retained");
    };
    request.inbox.drain();
    assert!(
        request
            .inbox
            .next_due(
                LaneFrame {
                    segment: kithara_signal::SegmentId::FIRST,
                    frame: 0
                },
                1
            )
            .is_none(),
        "selection does not send a second seek"
    );
    due.defer();
    let current = queue
        .current_track()
        .expect("unchanged current track")
        .snapshot();
    assert_eq!(queue.current, Some(first));
    assert_eq!(current.item, outgoing.item);
    assert_eq!(current.position, outgoing.position);
    assert_eq!(current.mark, outgoing.mark);
    assert_eq!(current.pending_lane, outgoing.pending_lane);
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn background_loads_stay_within_max_concurrent_loads(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(&worker_owner.0, NonZeroUsize::new(2).expect("cap"), false);
    let ids = append(&mut queue, &mut rig, &dir, 5);
    let loads = hold_loads(&mut queue, &mut rig).0;
    assert_eq!(
        loads
            .iter()
            .filter(|(_, class)| *class == ServiceClass::Warm)
            .count(),
        1
    );
    assert_eq!(
        loads
            .iter()
            .filter(|(_, class)| *class == ServiceClass::Idle)
            .count(),
        2
    );
    for id in &ids[3..] {
        hang_tick!();
        assert_eq!(
            queue.track(*id).expect("record").status,
            TrackStatus::Pending
        );
        hang_reset!();
    }
    for id in &ids[1..3] {
        hang_tick!();
        let seq = load_seq(&queue, *id);
        let loaded = loaded_fixture(
            queue.worker().expect("fixture worker"),
            &mut rig,
            &dir,
            seq,
            FixtureSource::Synthetic,
        )
        .await;
        hang_reset!();
        answer(&mut queue, &mut rig, seq, loaded);
        assert!(
            hold_loads(&mut queue, &mut rig).0.is_empty(),
            "answered residents still count"
        );
    }
    select(&mut queue, &mut rig, ids[1]);
    let loads = hold_loads(&mut queue, &mut rig).0;
    assert_eq!(
        loads,
        vec![(load_seq(&queue, ids[3]), ServiceClass::Idle)],
        "only the next pending track opens"
    );
    assert_eq!(
        queue.track(ids[4]).expect("record").status,
        TrackStatus::Pending
    );
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn selecting_a_background_loaded_track_opens_nothing_new(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[1]);
    hold_loads(&mut queue, &mut rig);
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    answer(&mut queue, &mut rig, seq, loaded);
    select(&mut queue, &mut rig, ids[1]);
    let (loads, priorities) = hold_loads(&mut queue, &mut rig);
    assert!(loads.is_empty());
    assert_eq!(priorities, vec![ServiceClass::Warm]);
    let slot = queue
        .active
        .iter()
        .find(|active| active.item == ids[1])
        .expect("seated entry")
        .slot;
    rig.ring.publish().expect("publish attach");
    rig.inbox.drain();
    let mut scope = rig.inbox.scope(rig.scope).expect("deck scope");
    let due = scope
        .next_due(SessionFrame::new(0), 1)
        .expect("attach batch");
    assert!(
        due.commands()
            .iter()
            .any(|part| matches!(part, DeckPart::Attach { slot: named, .. } if *named == slot))
    );
    due.apply(());
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn a_select_is_not_starved_by_hung_background_loads(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 5);
    let loads = hold_loads(&mut queue, &mut rig).0;
    assert_eq!(
        loads
            .iter()
            .filter(|(_, class)| *class == ServiceClass::Idle)
            .count(),
        3
    );
    select(&mut queue, &mut rig, ids[4]);
    assert_eq!(
        hold_loads(&mut queue, &mut rig).0,
        vec![(load_seq(&queue, ids[4]), ServiceClass::Warm)]
    );
    assert_eq!(
        queue.track(ids[0]).expect("initial record").status,
        TrackStatus::Cancelled
    );
    for id in &ids[1..4] {
        assert_eq!(
            queue.track(*id).expect("hung record").status,
            TrackStatus::Loading
        );
    }
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn the_first_append_arms_a_paused_initial_load(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(&worker_owner.0, NonZeroUsize::new(3).expect("cap"), false);
    let id = append(&mut queue, &mut rig, &dir, 1)[0];
    let seq = load_seq(&queue, id);
    assert_eq!(
        hold_loads(&mut queue, &mut rig).0,
        vec![(seq, ServiceClass::Warm)]
    );
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    answer(&mut queue, &mut rig, seq, loaded);
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(id));
    assert!(matches!(
        queue
            .current_track()
            .expect("initial track")
            .snapshot()
            .status,
        PlayingStatus::Paused { .. }
    ));
    select(&mut queue, &mut rig, id);
    crate::mock::finish(&mut queue, &mut rig);
    assert!(matches!(
        queue
            .current_track()
            .expect("selected track")
            .snapshot()
            .status,
        PlayingStatus::Playing { .. }
    ));
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn the_first_append_plays_with_autoplay(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(&worker_owner.0, NonZeroUsize::new(3).expect("cap"), true);
    let id = append(&mut queue, &mut rig, &dir, 1)[0];
    let seq = load_seq(&queue, id);
    assert_eq!(
        hold_loads(&mut queue, &mut rig).0,
        vec![(seq, ServiceClass::Warm)]
    );
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    answer(&mut queue, &mut rig, seq, loaded);
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(id));
    assert!(matches!(
        queue
            .current_track()
            .expect("initial track")
            .snapshot()
            .status,
        PlayingStatus::Playing { .. }
    ));
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn a_select_supersedes_the_initial_load(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    select(&mut queue, &mut rig, ids[1]);
    assert_eq!(
        queue.track(ids[0]).expect("initial record").status,
        TrackStatus::Cancelled
    );
    assert_eq!(queue.target.expect("new target").to, ids[1]);
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn selecting_another_track_consumes_the_opened_initial_resource(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[0]);
    hold_loads(&mut queue, &mut rig);
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    answer(&mut queue, &mut rig, seq, loaded);
    select(&mut queue, &mut rig, ids[1]);
    assert_eq!(
        queue.track(ids[0]).expect("initial record").status,
        TrackStatus::Consumed
    );
    assert_eq!(queue.target.expect("new target").to, ids[1]);
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn an_unsupported_codec_fails_before_select(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[1]);
    let mut receiver = queue.subscribe::<QueueEvent>();
    assert!(
        hold_loads(&mut queue, &mut rig)
            .0
            .contains(&(seq, ServiceClass::Idle))
    );
    rig.opens
        .resume(seq, (), ())
        .expect("background open")
        .refuse(LoadRefusal::Open(DecodeError::UnsupportedCodec {
            codec: kithara_stream::AudioCodec::AacLc,
        }));
    settle_dispatcher(&mut queue, &mut rig);
    assert!(matches!(
        queue.track(ids[1]).expect("record").status,
        TrackStatus::Failed(_)
    ));
    assert!(events(&mut receiver).iter().any(|event| matches!(event, QueueEvent::TrackLoadFailed { id, auto_skipped: false, .. } if *id == ids[1])));
    assert!(
        queue
            .active
            .parked_iter()
            .all(|parked| parked.item != ids[1])
    );
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn a_track_setting_reaches_background_loaded_tracks(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    hold_loads(&mut queue, &mut rig);
    let seq = load_seq(&queue, ids[1]);
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    let mut due = rig.opens.resume(seq, (), ()).expect("held background open");
    let held = std::mem::take(due.commands_mut());
    due.apply(Dispatched::Loaded(loaded));
    settle_dispatcher(&mut queue, &mut rig);
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::ConfigureTrack(TrackSettingsChange::Speed(1.25), When::Next),
            out,
        )
    })
    .expect("configure every held track");
    let track = queue
        .tracks_active()
        .find(|track| track.snapshot().item == ids[1])
        .expect("background track");
    assert_eq!(track.projected().speed(), 1.25);
    drop(held);
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn a_selected_background_track_announces_loaded_once(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[1]);
    let mut receiver = queue.subscribe::<QueueEvent>();
    hold_loads(&mut queue, &mut rig);
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    answer(&mut queue, &mut rig, seq, loaded);
    select(&mut queue, &mut rig, ids[1]);
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(ids[1]));
    let metadata = queue
        .current_track()
        .expect("selected track")
        .snapshot()
        .metadata
        .clone();
    queue.announce_loaded(ids[1], &metadata);
    queue.publish();
    let announced = events(&mut receiver);
    assert_eq!(announced.iter().filter(|event| matches!(event, QueueEvent::TrackStatusChanged { id, status: TrackStatus::Loaded, .. } if *id == ids[1])).count(), 1);
    assert_eq!(
        announced
            .iter()
            .filter(|event| matches!(event, QueueEvent::NextTrackReady { id, .. } if *id == ids[1]))
            .count(),
        1
    );
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn a_failed_replacement_release_keeps_the_taken_background_track(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    queue.active = Slots::new(1);
    let ids = append(&mut queue, &mut rig, &dir, 3);
    hold_loads(&mut queue, &mut rig);
    for id in &ids[1..] {
        hang_tick!();
        let seq = load_seq(&queue, *id);
        let loaded = loaded_fixture(
            queue.worker().expect("fixture worker"),
            &mut rig,
            &dir,
            seq,
            FixtureSource::Synthetic,
        )
        .await;
        answer(&mut queue, &mut rig, seq, loaded);
        hang_reset!();
    }
    with_outbox(&mut queue, &mut rig, |queue, out| {
        let output = *out.pass().expect("pass").output;
        queue.load_track(ids[1], Role::Preloaded, Some(&output), out)
    })
    .expect("stage the first parked replacement");
    while rig.dispatcher.available() > 0 {
        hang_tick!();
        rig.dispatcher
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: Vec::new(),
                },
            )
            .expect("consume dispatcher credit");
        hang_reset!();
    }
    let seq = load_seq(&queue, ids[2]);
    let result = with_outbox(&mut queue, &mut rig, |queue, out| {
        let output = *out.pass().expect("pass").output;
        queue.load_track(ids[2], Role::Preloaded, Some(&output), out)
    });
    assert!(matches!(
        result,
        Err(QueueError::Play(PlayError::Full("dispatcher")))
    ));
    let parked = queue
        .active
        .parked_iter()
        .find(|parked| parked.item == ids[2])
        .expect("a refused staging retains its track");
    assert_eq!(parked.load, Some(LoadState::Attaching(seq)));
    assert_eq!(parked.track.snapshot().slot, None);
    assert_eq!(parked.track.snapshot().status, PlayingStatus::Loaded);
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn a_stale_load_completion_does_not_cancel_the_live_target(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[0]);
    hold_loads(&mut queue, &mut rig);
    let old = queue
        .active
        .position(|active| active.item == ids[0])
        .expect("old entry");
    select(&mut queue, &mut rig, ids[1]);
    assert_eq!(
        queue.track(ids[0]).expect("old record").status,
        TrackStatus::Cancelled
    );
    assert_eq!(
        queue
            .active
            .get(old)
            .expect("released open retains its answer route")
            .load,
        Some(LoadState::Opening(seq))
    );
    with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.transition_settled(
            old,
            &Settled::Applied {
                seq,
                at: SessionFrame::new(0),
            },
            out,
        )
    })
    .expect("stale completion is harmless");
    assert_eq!(queue.target.expect("the live target survives").to, ids[1]);
    assert_eq!(
        queue.track(ids[1]).expect("live record").status,
        TrackStatus::Loading
    );
    let seq = load_seq(&queue, ids[1]);
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    answer(&mut queue, &mut rig, seq, loaded);
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(ids[1]));
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn a_superseded_initial_open_releases_its_answered_lane(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[0]);
    hold_loads(&mut queue, &mut rig);
    select(&mut queue, &mut rig, ids[1]);
    assert_eq!(load_seq(&queue, ids[0]), seq);
    let loaded = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    let lane = loaded.lane;
    answer(&mut queue, &mut rig, seq, loaded);
    rig.opens.drain();
    let due = rig
        .opens
        .next_due((), 1)
        .expect("answered superseded lane is released");
    assert!(matches!(due.commands(), [DispatcherCommand::Release(named)] if *named == lane));
    due.apply(Dispatched::Released);
    assert!(queue.active.iter().all(|active| active.item != ids[0]));
    assert_eq!(
        queue.track(ids[0]).expect("cancelled record").status,
        TrackStatus::Cancelled
    );
    assert_eq!(queue.target.expect("selected target survives").to, ids[1]);
    drop((queue, rig));
    dir.finish().await;
}

fn fill_dispatcher(rig: &mut mock::DeckRig<TestPools>, resident: kithara_render::LaneId) {
    for _ in 0..rig.dispatcher.available() {
        rig.dispatcher
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DispatcherCommand::SetPriority(resident, ServiceClass::Warm)],
                },
            )
            .expect("fill dispatcher credits");
    }
}

#[kithara::test(tokio)]
#[case::admitted(false, false)]
#[case::full_dispatcher(true, false)]
#[case::closed_after_admission(false, true)]
#[case::closed_before_admission(true, true)]
async fn released_tracks_wait_for_source_retirement_through_the_queue(
    #[by_ref] worker_owner: &WorkerOwner,
    #[case] full: bool,
    #[case] closed: bool,
) {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(&worker_owner.0, NonZeroUsize::MIN, false);
    let id = append(&mut queue, &mut rig, &dir, 1)[0];
    select(&mut queue, &mut rig, id);
    let seq = load_seq(&queue, id);
    let loaded = loaded_fixture(
        queue.worker().expect("worker"),
        &mut rig,
        &dir,
        seq,
        FixtureSource::Synthetic,
    )
    .await;
    let resident = loaded.lane;
    answer_loaded(&mut queue, &mut rig, loaded);
    crate::mock::settle(&mut queue, &mut rig, SessionFrame::new(0));
    assert_eq!(queue.active.len(), 1);
    assert!(queue.active.get(0).expect("loaded track").track.attached());
    if full {
        fill_dispatcher(&mut rig, resident);
        assert_eq!(rig.dispatcher.available(), 0);
    }
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::RemoveAll, out)
    })
    .expect("clear attached queue");
    crate::mock::settle(&mut queue, &mut rig, SessionFrame::new(0));
    assert_eq!(queue.active.len(), 0, "RT detach releases its slot");
    assert_eq!(
        queue.active.parked_len(),
        1,
        "source ownership survives RT detach"
    );
    if closed {
        let (_unused, replacement) = channel(ChannelConfig::builder().build());
        drop(std::mem::replace(&mut rig.opens, replacement));
        settle_dispatcher(&mut queue, &mut rig);
        tick_fixture(&mut queue, &mut rig);
        queue.reap_released();
        assert_eq!(
            queue.active.parked_len(),
            0,
            "a destroyed dispatcher cannot retain source retirement"
        );
        return;
    }
    let waiting = TrackId::allocate();
    queue.insert_entry(
        waiting,
        TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
        kithara_play::Position::ZERO,
        crate::queue::types::Placement::Append,
    );
    let output = mock::output(None).get();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.pump_loads(Some(&output), out);
    });
    assert!(
        !queue
            .active
            .parked_iter()
            .any(|parked| parked.item == waiting)
    );
    let mut release = None;
    rig.opens.drain();
    while let Some(due) = rig.opens.next_due((), 1) {
        match due.commands() {
            [DispatcherCommand::SetPriority(_, _)] => due.apply(Dispatched::Prioritized),
            [DispatcherCommand::Release(id)] if *id == resident => release = Some(due.defer()),
            _ => panic!("retirement or priority"),
        }
    }
    settle_dispatcher(&mut queue, &mut rig);
    tick_fixture(&mut queue, &mut rig);
    rig.opens.drain();
    if release.is_none() {
        let due = rig
            .opens
            .next_due((), 1)
            .expect("release retries after credits return");
        assert!(matches!(due.commands(), [DispatcherCommand::Release(id)] if *id == resident));
        release = Some(due.defer());
    }
    queue.reap_released();
    assert_eq!(
        queue.active.parked_len(),
        1,
        "a sent release still waits for its acknowledgement"
    );
    fill_dispatcher(&mut rig, resident);
    assert!(
        with_outbox(&mut queue, &mut rig, Queue::release_all).is_ok(),
        "an admitted release spends no new dispatcher credit"
    );
    rig.opens.drain();
    while let Some(due) = rig.opens.next_due((), 1) {
        assert!(matches!(
            due.commands(),
            [DispatcherCommand::SetPriority(_, _)]
        ));
        due.apply(Dispatched::Prioritized);
    }
    settle_dispatcher(&mut queue, &mut rig);
    rig.opens
        .resume(release.expect("source release"), (), ())
        .expect("held release")
        .apply(Dispatched::Released);
    let receipt = rig
        .dispatcher
        .receipts()
        .next()
        .expect("source acknowledgement");
    with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.settle_with_output(TrackReceipt::Loaded(receipt), Some(&output), out)
    });
    assert!(!queue.active.parked_iter().any(|parked| parked.item == id));
    assert!(
        queue
            .active
            .parked_iter()
            .any(|parked| parked.item == waiting
                && matches!(parked.load, Some(LoadState::Opening(_)))),
        "cleanup acknowledgement refills the released source budget without another command"
    );
}
