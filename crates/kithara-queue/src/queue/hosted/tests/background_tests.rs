use std::num::NonZeroUsize;

use kithara_command::Batch;
use kithara_decode::DecodeError;
use kithara_events::EventReceiver;
use kithara_play::{LoadRefusal, Settled, Track, TrackSettingsChange};
use kithara_render::{Dispatched, DispatcherCommand, Loaded, ServiceClass};

use super::*;
use crate::{
    Transition,
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
                        id,
                        source: TrackSource::Uri(
                            dir.path()
                                .join("entry.wav")
                                .to_str()
                                .expect("fixture path")
                                .to_owned(),
                        ),
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

fn load_seq(queue: &Queue<TestPools>, id: TrackId) -> Seq {
    queue
        .active
        .iter()
        .find(|active| active.item == id)
        .and_then(|active| active.load)
        .or_else(|| {
            queue
                .active
                .parked_iter()
                .find(|parked| parked.item == id)
                .and_then(|parked| parked.load)
        })
        .expect("track has a load")
        .seq()
}

fn settle_dispatcher(queue: &mut Queue<TestPools>, rig: &mut mock::DeckRig<TestPools>) {
    let receipts = rig.dispatcher.receipts().collect::<Vec<_>>();
    for receipt in receipts {
        with_outbox(queue, rig, |queue, out| {
            Player::settle(queue, TrackReceipt::Loaded(receipt), out)
        });
    }
}

fn hold_loads(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
) -> (Vec<(Seq, ServiceClass)>, Vec<ServiceClass>) {
    let mut loads = Vec::new();
    let mut priorities = Vec::new();
    rig.opens.drain();
    while let Some(due) = rig.opens.next_due((), 1) {
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

fn events(receiver: &mut EventReceiver<QueueEvent>) -> Vec<QueueEvent> {
    std::iter::from_fn(|| receiver.try_recv().ok().map(|envelope| envelope.event)).collect()
}

#[kithara::test(tokio)]
async fn prefetch_lane_caps_concurrent_loads() {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(NonZeroUsize::new(2).expect("cap"), false);
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
        let loaded = loaded_fixture(&mut rig, &dir).await;
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
}

#[kithara::test(tokio)]
async fn spawn_load_bad_url_emits_failed_status() {
    let (mut queue, mut rig, dir) = empty_queue();
    append(&mut queue, &mut rig, &dir, 1);
    let id = TrackId::allocate();
    let mut receiver = queue.subscribe::<QueueEvent>();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
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
}

#[kithara::test(tokio)]
async fn an_appended_track_loads_without_a_select() {
    let (mut queue, mut rig, dir) = empty_queue();
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[1]);
    let mut receiver = queue.subscribe::<QueueEvent>();
    let loads = hold_loads(&mut queue, &mut rig).0;
    assert!(loads.contains(&(seq, ServiceClass::Idle)));
    let loaded = loaded_fixture(&mut rig, &dir).await;
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
}

#[kithara::test(tokio)]
async fn background_loads_stay_within_max_concurrent_loads() {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(NonZeroUsize::new(2).expect("cap"), false);
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
        assert_eq!(
            queue.track(*id).expect("record").status,
            TrackStatus::Pending
        );
    }
    for id in &ids[1..3] {
        let seq = load_seq(&queue, *id);
        let loaded = loaded_fixture(&mut rig, &dir).await;
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
}

#[kithara::test(tokio)]
async fn selecting_a_background_loaded_track_opens_nothing_new() {
    let (mut queue, mut rig, dir) = empty_queue();
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[1]);
    hold_loads(&mut queue, &mut rig);
    let loaded = loaded_fixture(&mut rig, &dir).await;
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
}

#[kithara::test(tokio)]
async fn a_select_is_not_starved_by_hung_background_loads() {
    let (mut queue, mut rig, dir) = empty_queue();
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
}

#[kithara::test(tokio)]
async fn the_first_append_arms_a_paused_initial_load() {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(NonZeroUsize::new(3).expect("cap"), false);
    let id = append(&mut queue, &mut rig, &dir, 1)[0];
    let seq = load_seq(&queue, id);
    assert_eq!(
        hold_loads(&mut queue, &mut rig).0,
        vec![(seq, ServiceClass::Warm)]
    );
    let loaded = loaded_fixture(&mut rig, &dir).await;
    answer(&mut queue, &mut rig, seq, loaded);
    player_internal::finish(&mut queue, &mut rig);
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
    player_internal::finish(&mut queue, &mut rig);
    assert!(matches!(
        queue
            .current_track()
            .expect("selected track")
            .snapshot()
            .status,
        PlayingStatus::Playing { .. }
    ));
}

#[kithara::test(tokio)]
async fn the_first_append_plays_with_autoplay() {
    let (mut queue, mut rig, dir) =
        queue_with_load_settings(NonZeroUsize::new(3).expect("cap"), true);
    let id = append(&mut queue, &mut rig, &dir, 1)[0];
    let seq = load_seq(&queue, id);
    assert_eq!(
        hold_loads(&mut queue, &mut rig).0,
        vec![(seq, ServiceClass::Warm)]
    );
    let loaded = loaded_fixture(&mut rig, &dir).await;
    answer(&mut queue, &mut rig, seq, loaded);
    player_internal::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(id));
    assert!(matches!(
        queue
            .current_track()
            .expect("initial track")
            .snapshot()
            .status,
        PlayingStatus::Playing { .. }
    ));
}

#[kithara::test(tokio)]
async fn a_select_supersedes_the_initial_load() {
    let (mut queue, mut rig, dir) = empty_queue();
    let ids = append(&mut queue, &mut rig, &dir, 2);
    select(&mut queue, &mut rig, ids[1]);
    assert_eq!(
        queue.track(ids[0]).expect("initial record").status,
        TrackStatus::Cancelled
    );
    assert_eq!(queue.target.expect("new target").to, ids[1]);
}

#[kithara::test(tokio)]
async fn an_unsupported_codec_fails_before_select() {
    let (mut queue, mut rig, dir) = empty_queue();
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
}

#[kithara::test(tokio)]
async fn a_track_setting_reaches_background_loaded_tracks() {
    let (mut queue, mut rig, dir) = empty_queue();
    let ids = append(&mut queue, &mut rig, &dir, 2);
    hold_loads(&mut queue, &mut rig);
    let loaded = loaded_fixture(&mut rig, &dir).await;
    let seq = load_seq(&queue, ids[1]);
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
}

#[kithara::test(tokio)]
async fn a_selected_background_track_announces_loaded_once() {
    let (mut queue, mut rig, dir) = empty_queue();
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[1]);
    let mut receiver = queue.subscribe::<QueueEvent>();
    hold_loads(&mut queue, &mut rig);
    let loaded = loaded_fixture(&mut rig, &dir).await;
    answer(&mut queue, &mut rig, seq, loaded);
    select(&mut queue, &mut rig, ids[1]);
    player_internal::finish(&mut queue, &mut rig);
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
}

#[kithara::test(tokio)]
async fn a_failed_replacement_release_keeps_the_taken_background_track() {
    let (mut queue, mut rig, dir) = empty_queue();
    queue.active = Slots::new(1);
    let ids = append(&mut queue, &mut rig, &dir, 3);
    hold_loads(&mut queue, &mut rig);
    for id in &ids[1..] {
        let seq = load_seq(&queue, *id);
        let loaded = loaded_fixture(&mut rig, &dir).await;
        answer(&mut queue, &mut rig, seq, loaded);
    }
    with_outbox(&mut queue, &mut rig, |queue, out| {
        let output = *out.pass().expect("pass").output;
        queue.load_track(ids[1], Role::Preloaded, Some(&output), out)
    })
    .expect("stage the first parked replacement");
    while rig.dispatcher.available() > 0 {
        rig.dispatcher
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: Vec::new(),
                },
            )
            .expect("consume dispatcher credit");
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
}

#[kithara::test(tokio)]
async fn a_stale_load_completion_does_not_cancel_the_live_target() {
    let (mut queue, mut rig, dir) = empty_queue();
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
    let loaded = loaded_fixture(&mut rig, &dir).await;
    answer(&mut queue, &mut rig, seq, loaded);
    player_internal::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(ids[1]));
}

#[kithara::test(tokio)]
async fn a_superseded_initial_open_releases_its_answered_lane() {
    let (mut queue, mut rig, dir) = empty_queue();
    let ids = append(&mut queue, &mut rig, &dir, 2);
    let seq = load_seq(&queue, ids[0]);
    hold_loads(&mut queue, &mut rig);
    select(&mut queue, &mut rig, ids[1]);
    assert_eq!(load_seq(&queue, ids[0]), seq);
    let loaded = loaded_fixture(&mut rig, &dir).await;
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
}
