use kithara_events::EventReceiver;
use kithara_play::{PlayerEvent, PlayerStatus, TrackSettingsChange, mock};

use super::*;
use crate::{
    QueueError, Transition,
    mock::{finish, fixture_uri, selected},
};

fn apply(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    command: QueueCommand<TestPools>,
) -> Result<Option<Seq>, PlayError> {
    with_outbox(queue, rig, |queue, out| Player::apply(queue, command, out))
}

async fn pending_successor() -> (
    Queue<TestPools>,
    TrackId,
    TrackId,
    mock::DeckRig<TestPools>,
    TestTempDir,
) {
    let (mut queue, first, second, mut rig, dir) = selected().await;
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("select successor");
    (queue, first, second, rig, dir)
}

fn drain(rx: &mut EventReceiver<QueueEvent>) -> Vec<QueueEvent> {
    std::iter::from_fn(|| rx.try_recv().ok().map(|envelope| envelope.event)).collect()
}

#[kithara::test(tokio)]
async fn rate_events_follow_applied_playback_without_duplicates() {
    let (mut queue, _, _, mut rig, dir) = pending_selection();
    let control = queue.control();
    let mut events = queue.subscribe::<PlayerEvent>();
    let rates = |events: &mut EventReceiver<PlayerEvent>| {
        std::iter::from_fn(|| events.try_recv().ok().map(|event| event.event))
            .filter_map(|event| match event {
                PlayerEvent::RateChanged { rate } => Some(rate),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(control.rate(), 0.0);
    answer_load(&mut queue, &mut rig, &dir).await;
    assert!(rates(&mut events).is_empty());
    finish(&mut queue, &mut rig);
    assert_eq!(control.rate(), 1.0);
    assert_eq!(rates(&mut events), [1.0]);

    apply(&mut queue, &mut rig, QueueCommand::Pause { at: When::Next }).expect("pause accepted");
    assert_eq!(control.rate(), 1.0);
    assert!(rates(&mut events).is_empty());
    crate::mock::settle(&mut queue, &mut rig, SessionFrame::new(384));
    assert_eq!(control.rate(), 0.0);
    assert_eq!(rates(&mut events), [0.0]);
    tick_fixture(&mut queue, &mut rig);
    assert!(rates(&mut events).is_empty());

    apply(
        &mut queue,
        &mut rig,
        QueueCommand::ConfigureTrack(TrackSettingsChange::Speed(2.0), When::Next),
    )
    .expect("paused speed accepted");
    assert_eq!(control.rate(), 0.0);
    assert!(rates(&mut events).is_empty());
    crate::mock::settle(&mut queue, &mut rig, SessionFrame::new(512));
    assert_eq!(control.rate(), 0.0);
    assert!(rates(&mut events).is_empty());

    apply(&mut queue, &mut rig, QueueCommand::Play { at: When::Next }).expect("resume accepted");
    assert_eq!(control.rate(), 0.0);
    assert!(rates(&mut events).is_empty());
    let mut frame = 640;
    kithara_test_utils::wait_until(Duration::from_secs(1), "resumed rate receipt", || {
        tick_fixture(&mut queue, &mut rig);
        crate::mock::settle(&mut queue, &mut rig, SessionFrame::new(frame));
        frame += 128;
        tick_fixture(&mut queue, &mut rig);
        control.rate() == 2.0
    })
    .await
    .expect("the resumed segment becomes ready and its Start receipt applies");
    assert_eq!(control.rate(), 2.0);
    assert_eq!(rates(&mut events), [2.0]);
    tick_fixture(&mut queue, &mut rig);
    assert!(rates(&mut events).is_empty());
}

#[kithara::test(tokio)]
#[case(false)]
#[case(true)]
async fn player_remove_all_resets_state(#[case] with_item: bool) {
    let (mut queue, _, _, mut rig, dir) = pending_selection();
    if with_item {
        apply(&mut queue, &mut rig, QueueCommand::Pause { at: When::Next })
            .expect("pause selection");
        answer_load(&mut queue, &mut rig, &dir).await;
        finish(&mut queue, &mut rig);
        assert!(queue.control().current().is_some());
    }
    apply(&mut queue, &mut rig, QueueCommand::RemoveAll).expect("clear");
    finish(&mut queue, &mut rig);
    assert_eq!(queue.current, None);
    assert_eq!(queue.control().status(), PlayerStatus::Unknown);
}

#[kithara::test(tokio)]
async fn replay_same_item_does_not_re_emit_current_item_changed() {
    let (mut queue, _, _, mut rig, dir) = pending_selection();
    let mut rx = queue.control().subscribe();
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    let first = drain(&mut rx);
    let first_count = first
        .iter()
        .filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. }))
        .count();
    assert_eq!(
        first_count, 1,
        "selecting the item announces it once: {first:?}"
    );
    apply(&mut queue, &mut rig, QueueCommand::Play { at: When::Next }).expect("resume");
    finish(&mut queue, &mut rig);
    let second = drain(&mut rx);
    let second_count = second
        .iter()
        .filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. }))
        .count();
    assert_eq!(
        second_count, 0,
        "resuming the same item must not re-announce CurrentItemChanged: {second:?}"
    );
}

#[kithara::test(tokio)]
async fn a_selected_resource_adopts_the_session_wake_mode() {
    use kithara_render::{LoadRequest, ServiceClass};

    let pools = pools();
    let dir = TestTempDir::new();
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(
            PlayWorkerConfig::builder(pools.clone()).build(),
        ))
        .build();
    let output = mock::output(None).get();
    assert_eq!(output.sample_rate.output(), 44_100);
    let source = fixture_uri(dir.path(), "selected.wav");
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();
    crate::mock::seed_fixture(
        &store,
        &source,
        kithara_test_fixtures::assets::fixed_constant_wav_cd_half_4096().bytes(),
    );
    let config = ResourceConfig::for_src(ResourceSrc::parse(&source).expect("fixture URI"))
        .downloader(crate::mock::downloader())
        .store(store)
        .build();
    let prepared = prep.prepare(config, &output).expect("prepared source");
    let bus = prepared.bus().expect("prepared event bus").clone();
    let item = kithara_play::ResourceLoad::new(
        prepared,
        Box::new(kithara_audio::AudioObserverSlot::default().relay()),
    );
    let (_control, inbox) = prep.worker.lane_channel();
    mock::assert_requested_render_off_bus(
        LoadRequest {
            item,
            class: ServiceClass::Warm,
            inbox,
            position: Duration::ZERO,
            start: kithara_play::TrackSettings::default().lane_start(),
        },
        &bus,
        &output,
        &pools,
    )
    .await
    .expect("selected lane renders off the bus");
}

#[kithara::test(tokio)]
async fn re_selecting_the_current_item_does_not_re_announce() {
    let (mut queue, item, _, mut rig, _dir) = selected().await;
    let mut rx = queue.control().subscribe();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: item,
            transition: Transition::None,
        },
    )
    .expect("re-select current");
    apply(&mut queue, &mut rig, QueueCommand::Pause { at: When::Next }).expect("pause current");
    finish(&mut queue, &mut rig);
    let after = drain(&mut rx);
    let announces = after
        .iter()
        .filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. }))
        .count();
    assert_eq!(
        announces, 0,
        "re-selecting the current item must not re-announce: {after:?}"
    );
}

#[kithara::test(tokio)]
async fn selecting_another_item_announces_it() {
    let (mut queue, _, second, mut rig, dir) = selected().await;
    let mut rx = queue.control().subscribe();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("select second");
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    let after = drain(&mut rx);
    assert!(
        matches!(after.iter().filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. })).collect::<Vec<_>>().as_slice(),
        [QueueEvent::CurrentTrackChanged { id: Some(announced) }] if *announced == second),
        "selecting another item must announce it once: {after:?}"
    );
    assert_eq!(queue.current, Some(second));
}

#[kithara::test(tokio)]
async fn arm_next_arms_the_item() {
    let (queue, _, second, _rig, _dir) = pending_successor().await;
    assert_eq!(queue.target.map(|target| target.to), Some(second));
}

#[kithara::test(tokio)]
async fn seek_seconds_updates_position_optimistically() {
    let (mut queue, _, _, mut rig, _dir) = pending_selection();
    apply(&mut queue, &mut rig, QueueCommand::RemoveAll).expect("empty queue");
    let to = Duration::from_secs_f64(54.689_879_542);
    let outcome = apply(&mut queue, &mut rig, QueueCommand::Seek { to });
    assert!(matches!(outcome, Ok(None)));
    assert_eq!(queue.control().position_seconds(), Some(54.689_879_542));
}

#[kithara::test(tokio)]
async fn arm_next_idempotent_for_the_armed_item() {
    let (mut queue, _, second, mut rig, _dir) = pending_successor().await;
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("select same target");
    assert_eq!(queue.target.map(|target| target.to), Some(second));
}

#[kithara::test(tokio)]
async fn arm_next_replaces_a_previously_armed_item() {
    let (mut queue, _, _, mut rig, dir) = pending_successor().await;
    let third = TrackId::allocate();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id: third,
            source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
        },
    )
    .expect("append third");
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: third,
            transition: Transition::None,
        },
    )
    .expect("select third");
    assert_eq!(queue.target.map(|target| target.to), Some(third));
}

#[kithara::test(tokio)]
async fn commit_next_of_another_item_returns_typed_error() {
    let (mut queue, _, second, mut rig, _dir) = pending_successor().await;
    let other = TrackId::allocate();
    let err = with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.apply_command(
            QueueCommand::Select {
                id: other,
                transition: Transition::Crossfade,
            },
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect_err("mismatch");
    assert!(
        matches!(err, QueueError::UnknownTrackId(requested) if requested == other)
            && queue.target.is_some_and(|target| target.to == second)
    );
}

#[kithara::test(tokio)]
async fn commit_next_makes_the_successor_current_and_announces_it() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    let mut rx = queue.control().subscribe();
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(second));
    assert_eq!(
        queue.target.map(|target| target.to),
        None,
        "armed clears after commit"
    );
    let announced = drain(&mut rx).iter().any(|event| matches!(event, QueueEvent::CurrentTrackChanged { id: Some(item) } if *item == second));
    assert!(announced, "commit_next must publish CurrentItemChanged");
}

#[kithara::test(tokio)]
async fn commit_next_idempotent_when_already_activated() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    for _ in 0..2 {
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Select {
                id: second,
                transition: Transition::Crossfade,
            },
        )
        .expect("select current");
        finish(&mut queue, &mut rig);
    }
    assert_eq!(queue.current, Some(second));
}

#[kithara::test(tokio)]
async fn unarm_next_clears_an_armed_successor() {
    let (mut queue, first, _, mut rig, _dir) = pending_successor().await;
    with_outbox(&mut queue, &mut rig, Queue::cancel_target).expect("cancel pending transition");
    assert_eq!(queue.target.map(|target| target.to), None);
    assert_eq!(queue.current, Some(first));
}

#[kithara::test(tokio)]
async fn unarm_next_preserves_activated_current() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    with_outbox(&mut queue, &mut rig, Queue::cancel_target).expect("cancel no pending target");
    assert_eq!(queue.target.map(|target| target.to), None);
    assert_eq!(queue.current, Some(second));
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn selecting_another_item_unarms_the_successor() {
    hang_tick!();
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    hang_reset!();
    hang_tick!();
    answer_load(&mut queue, &mut rig, &dir).await;
    hang_reset!();
    let third = TrackId::allocate();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id: third,
            source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
        },
    )
    .expect("append third");
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: third,
            transition: Transition::None,
        },
    )
    .expect("select third");
    finish(&mut queue, &mut rig);
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    assert_eq!(
        queue.target.map(|target| target.to),
        None,
        "select must unarm"
    );
    assert_eq!(queue.current, Some(third));
    assert_ne!(queue.current, Some(second));
}

#[kithara::test(tokio)]
async fn selecting_the_armed_item_promotes_it() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    answer_load(&mut queue, &mut rig, &dir).await;
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("select armed target");
    finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(second));
    assert_eq!(
        queue.target.map(|target| target.to),
        None,
        "armed track consumed by select"
    );
}
