use kithara_command::Port;
use kithara_config::ConfigOwner;
use kithara_events::EventReceiver;
use kithara_platform::{CancelToken, time::Duration};
use kithara_play::TrackSettingsChange;
use kithara_test_utils::cancel_token;

use super::*;
use crate::{
    NavigationState, QueueSettingsChange, Transition, mock::fixture_uri, queue::types::Placement,
};

fn apply(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    command: QueueCommand<TestPools>,
) -> Result<Option<Seq>, PlayError> {
    with_outbox(queue, rig, |queue, out| Player::apply(queue, command, out))
}

fn apply_mix(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    mix: &mut kithara_command::Live<DeckMixSettings, kithara_play::DeckProtocol>,
    command: QueueCommand<TestPools>,
) -> Result<Option<Seq>, PlayError> {
    let output = mock::output(None).get();
    let deck = queue.deck.mixer.clone();
    let pass = DeckPass {
        mix: *mix.config(),
        suspended: false,
        now: SessionFrame::new(256),
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

#[kithara::hang_watchdog]
fn changes(events: &mut EventReceiver<QueueEvent>) -> Vec<Option<TrackId>> {
    let mut changes = Vec::new();
    while let Ok(envelope) = events.try_recv() {
        hang_tick!();
        if let QueueEvent::CurrentTrackChanged { id } = envelope.event {
            changes.push(id);
        }
        hang_reset!();
    }
    changes
}

#[kithara::test]
fn advance_stops_at_end() {
    let ids = [TrackId::allocate(), TrackId::allocate()];
    let mut navigation = NavigationState::new(2);
    let mut current = Some(ids[0]);
    navigation.commit(None, ids[0], crate::AdvanceReason::UserSelect, &ids);
    assert_eq!(navigation.next(&ids, current, false, false), Some(ids[1]));
    navigation.commit(current, ids[1], crate::AdvanceReason::UserNext, &ids);
    current = Some(ids[1]);
    assert_eq!(navigation.next(&ids, current, false, false), None);
    assert_eq!(current, Some(ids[1]));
}

#[kithara::test(tokio)]
async fn insert_and_remove_preserve_resource() {
    let (mut queue, _, _dir) = empty_queue();
    let id = TrackId::allocate();
    queue.insert_entry(
        id,
        TrackSource::Uri("first".to_owned()),
        kithara_play::Position::ZERO,
        Placement::Append,
    );
    let removed = queue.tracks.records_mut().remove(0);
    assert_eq!(removed.source.uri(), Some("first"));
    assert_eq!(queue.tracks.records().len(), 0);
}

#[kithara::test(tokio)]
#[case(false)]
#[case(true)]
async fn announce_carries_the_item_identity(#[case] loaded: bool) {
    let (mut queue, mut rig, dir) = empty_queue();
    let id = TrackId::from(7_u64);
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id,
            source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
        },
    )
    .expect("insert identity");
    let mut events = queue.control().subscribe();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id,
            transition: Transition::None,
        },
    )
    .expect("select identity");
    if loaded {
        answer_load(&mut queue, &mut rig, &dir).await;
        crate::mock::finish(&mut queue, &mut rig);
    } else {
        let incoming = queue
            .incoming_index(id)
            .expect("identity before resource open");
        assert!(matches!(
            queue
                .active
                .get(incoming)
                .expect("queued identity")
                .track
                .snapshot()
                .as_ref()
                .status,
            PlayingStatus::Loading
        ));
        queue
            .enter_target(
                incoming,
                SessionFrame::new(0),
                crate::AdvanceReason::UserSelect,
            )
            .expect("announce queued identity");
        queue.publish();
    }
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id,
            transition: Transition::None,
        },
    )
    .expect("repeat selection");
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(changes(&mut events), [Some(id)]);
    assert!(events.try_recv().is_err());
}

#[kithara::test(tokio)]
async fn clear_resets_cursor_and_announce() {
    let (mut queue, first, second, mut rig, dir) = crate::mock::selected().await;
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
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.control().current_index(), Some(1));
    apply(&mut queue, &mut rig, QueueCommand::RemoveAll).expect("clear");
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, None);
    assert_eq!(queue.tracks.records().len(), 0);
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id: first,
            source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
        },
    )
    .expect("reinsert first");
    let mut events = queue.control().subscribe();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: first,
            transition: Transition::None,
        },
    )
    .expect("select fresh first");
    answer_load(&mut queue, &mut rig, &dir).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.control().current_index(), Some(0));
    assert_eq!(changes(&mut events), [Some(first)]);
}

#[kithara::test(tokio)]
async fn mark_announced_uses_swap_semantics() {
    let (mut queue, first, second, mut rig, dir) = pending_selection();
    let mut events = queue.control().subscribe();
    answer_load(&mut queue, &mut rig, &dir).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(changes(&mut events), [Some(first)]);
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: first,
            transition: Transition::None,
        },
    )
    .expect("same item");
    crate::mock::finish(&mut queue, &mut rig);
    assert!(changes(&mut events).is_empty());
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("different item");
    answer_load(&mut queue, &mut rig, &dir).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(changes(&mut events), [Some(second)]);
}

#[kithara::test(tokio)]
async fn player_advance_emits_event() {
    let (mut queue, _, second, mut rig, dir) = crate::mock::selected().await;
    let mut events = queue.control().subscribe();
    apply(&mut queue, &mut rig, QueueCommand::Next(Transition::None)).expect("next");
    answer_load(&mut queue, &mut rig, &dir).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert!(matches!(changes(&mut events).as_slice(), [Some(id)] if *id == second));
}

#[kithara::test(tokio)]
async fn player_advance_through_queue() {
    let (mut queue, _, second, mut rig, dir) = crate::mock::selected().await;
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
    .expect("third");
    assert_eq!(queue.control().current_index(), Some(0));
    apply(&mut queue, &mut rig, QueueCommand::Next(Transition::None)).expect("second");
    answer_load(&mut queue, &mut rig, &dir).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.control().current_index(), Some(1));
    assert_eq!(queue.current, Some(second));
    apply(&mut queue, &mut rig, QueueCommand::Next(Transition::None)).expect("third");
    answer_load(&mut queue, &mut rig, &dir).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.control().current_index(), Some(2));
    apply(&mut queue, &mut rig, QueueCommand::Next(Transition::None)).expect("exhausted");
    assert_eq!(queue.control().current_index(), Some(2));
}

#[kithara::test(tokio)]
#[case(false, 2)]
#[case(true, 3)]
async fn player_insert_scenarios(#[case] insert_at_position: bool, #[case] expected_count: usize) {
    let (mut queue, mut rig, dir) = empty_queue();
    for _ in 0..2 {
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Append {
                position: kithara_play::Position::ZERO,
                id: TrackId::allocate(),
                source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
            },
        )
        .expect("append");
    }
    if insert_at_position {
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Insert {
                id: TrackId::allocate(),
                source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
                after: None,
            },
        )
        .expect("insert first");
    }
    assert_eq!(queue.tracks.records().len(), expected_count);
}

#[kithara::test(tokio)]
#[case::existing_item(0)]
#[case::out_of_bounds(1)]
#[case::shift_current_index(2)]
async fn player_remove_at_scenarios(#[case] scenario: u8) {
    let (mut queue, first, second, mut rig, dir) = crate::mock::selected().await;
    if scenario == 1 {
        apply(&mut queue, &mut rig, QueueCommand::Remove(second)).expect("one item");
        let unknown = TrackId::from(5_u64);
        assert!(apply(&mut queue, &mut rig, QueueCommand::Remove(unknown)).is_err());
        assert_eq!(queue.tracks.records().len(), 1);
    } else if scenario == 0 {
        assert!(apply(&mut queue, &mut rig, QueueCommand::Remove(first)).is_ok());
        assert_eq!(queue.tracks.records().len(), 1);
    } else {
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
        .expect("third");
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Select {
                id: third,
                transition: Transition::None,
            },
        )
        .expect("select third");
        answer_load(&mut queue, &mut rig, &dir).await;
        crate::mock::finish(&mut queue, &mut rig);
        assert_eq!(queue.control().current_index(), Some(2));
        apply(&mut queue, &mut rig, QueueCommand::Remove(first)).expect("remove earlier");
        assert_eq!(queue.control().current_index(), Some(1));
        assert_eq!(queue.tracks.records().len(), 2);
    }
}

#[kithara::test(tokio)]
async fn cold_navigation_and_removal_preserve_item_identity() {
    for count in [2, 3] {
        let (mut queue, mut rig, dir) = empty_queue();
        let ids = [
            TrackId::allocate(),
            TrackId::allocate(),
            TrackId::allocate(),
        ];
        let source = fixture_uri(dir.path(), "entry.wav");
        for &id in &ids[..count] {
            apply(
                &mut queue,
                &mut rig,
                QueueCommand::Append {
                    position: kithara_play::Position::ZERO,
                    id,
                    source: TrackSource::Uri(source.clone()),
                },
            )
            .expect("append cold item");
        }
        let before: Vec<_> = queue
            .control()
            .tracks()
            .iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(before, ids[..count]);
        assert_eq!(queue.control().current_index(), None);
        let unknown = TrackId::allocate();
        assert!(matches!(
            apply(&mut queue, &mut rig, QueueCommand::Remove(unknown)),
            Err(PlayError::ItemConsumed { item }) if item == unknown
        ));
        assert_eq!(
            queue
                .control()
                .tracks()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            before
        );
        assert_eq!(queue.control().current_index(), None);
        if count == 2 {
            apply(&mut queue, &mut rig, QueueCommand::Remove(ids[0])).expect("remove cold first");
            assert_eq!(queue.control().tracks().len(), 1);
            assert_eq!(queue.control().tracks()[0].id, ids[1]);
            assert_eq!(queue.control().current_index(), None);
            continue;
        }
        answer_load(&mut queue, &mut rig, &dir).await;
        crate::mock::finish(&mut queue, &mut rig);
        assert_eq!(queue.control().current_index(), Some(0));
        assert_eq!(queue.control().current().expect("initial item").id, ids[0]);
        for (index, &id) in ids.iter().enumerate().skip(1) {
            apply(&mut queue, &mut rig, QueueCommand::Next(Transition::None)).expect("next");
            answer_load(&mut queue, &mut rig, &dir).await;
            crate::mock::finish(&mut queue, &mut rig);
            assert_eq!(queue.control().current_index(), Some(index));
            assert_eq!(queue.control().current().expect("advanced item").id, id);
        }
        assert_eq!(queue.control().current_index(), Some(2));
        apply(&mut queue, &mut rig, QueueCommand::Remove(ids[0])).expect("remove earlier");
        assert_eq!(queue.control().current_index(), Some(1));
        assert_eq!(
            queue.control().current().expect("retained current").id,
            ids[2]
        );
        assert_eq!(queue.control().tracks().len(), 2);
        assert_eq!(
            queue
                .control()
                .tracks()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            ids[1..]
        );
        apply(&mut queue, &mut rig, QueueCommand::Remove(ids[2])).expect("remove current");
        assert_eq!(queue.control().tracks().len(), 1);
        assert_eq!(queue.control().tracks()[0].id, ids[1]);
    }
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn remove_at_shifts_current_and_reopens_announce() {
    hang_tick!();
    let (mut queue, first, _, mut rig, dir) = crate::mock::selected().await;
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
    .expect("third");
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: third,
            transition: Transition::None,
        },
    )
    .expect("select third");
    hang_tick!();
    answer_load(&mut queue, &mut rig, &dir).await;
    hang_reset!();
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.control().current_index(), Some(2));
    let mut events = queue.control().subscribe();
    apply(&mut queue, &mut rig, QueueCommand::Remove(first)).expect("remove first");
    assert_eq!(queue.control().current_index(), Some(1));
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: third,
            transition: Transition::None,
        },
    )
    .expect("same identity at shifted position");
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(changes(&mut events), [Some(third)]);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn replacing_current_item_re_announces_on_next_play() {
    hang_tick!();
    let (mut queue, first, _, mut rig, dir) = crate::mock::selected().await;
    hang_reset!();
    let replacement = TrackId::allocate();
    assert_ne!(replacement, first);
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Insert {
            id: replacement,
            source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
            after: None,
        },
    )
    .expect("replacement at index zero");
    apply(&mut queue, &mut rig, QueueCommand::Remove(first)).expect("remove replaced item");
    crate::mock::finish(&mut queue, &mut rig);
    let mut events = queue.control().subscribe();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: replacement,
            transition: Transition::None,
        },
    )
    .expect("play replacement");
    hang_tick!();
    answer_load(&mut queue, &mut rig, &dir).await;
    hang_reset!();
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.control().current_index(), Some(0));
    assert_eq!(changes(&mut events), [Some(replacement)]);
    assert_eq!(queue.tracks.records().len(), 2);
}

#[kithara::test(tokio)]
async fn select_item_out_of_range_returns_typed_error() {
    let (mut queue, mut rig, _dir) = empty_queue();
    let id = TrackId::from(5_u64);
    let err = with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.apply_command(
            QueueCommand::Select {
                id,
                transition: Transition::None,
            },
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect_err("unknown item");
    assert!(matches!(err, QueueError::UnknownTrackId(actual) if actual == id));
    assert_eq!(queue.tracks.records().len(), 0);
}

#[kithara::test(tokio)]
async fn checked_crossfade_update_respects_closed_owner() {
    let (mut queue, mut rig, _dir) = empty_queue();
    let settings = crate::CrossfadeSettings {
        duration: 2.0,
        ..Default::default()
    };
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::ConfigureQueue(QueueSettingsChange::Crossfade(settings), When::Next),
    )
    .expect("idle crossfade setting");
    assert_eq!(queue.config.settings.crossfade().duration, 2.0);
    with_outbox(&mut queue, &mut rig, |queue, out| {
        HostedDeck::close(queue, out)
    })
    .expect("close");
    let settings = crate::CrossfadeSettings {
        duration: 3.0,
        ..Default::default()
    };
    assert!(matches!(
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::ConfigureQueue(QueueSettingsChange::Crossfade(settings), When::Next)
        ),
        Err(PlayError::Closed)
    ));
    assert_eq!(queue.config.settings.crossfade().duration, 2.0);
}

#[kithara::test]
fn player_config_values_follow_live_controls() {
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let mut queue = Queue::new(
        QueueConfig::builder()
            .prep(prep)
            .preload_lead(Duration::from_secs(4))
            .settings(
                crate::QueueSettings::builder()
                    .gapless(false)
                    .crossfade(crate::CrossfadeSettings {
                        duration: 2.0,
                        ..Default::default()
                    })
                    .build(),
            )
            .build(),
    );
    let mut rig = mock::DeckRig::new(DeckMixerConfig::default()).expect("deck rig");
    assert!(!queue.config.settings.gapless());
    assert_eq!(queue.config.settings.crossfade().duration, 2.0);
    queue.publish();
    assert_eq!(queue.control().crossfade_settings().duration, 2.0);
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::ConfigureQueue(
            QueueSettingsChange::Crossfade(crate::CrossfadeSettings {
                duration: 3.0,
                ..Default::default()
            }),
            When::Next,
        ),
    )
    .expect("crossfade");
    assert_eq!(queue.control().crossfade_settings().duration, 3.0);
    assert_eq!(queue.config.settings.crossfade().duration, 3.0);
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::SetActionAtItemEnd(ActionAtItemEnd::None),
    )
    .expect("disable advance");
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::ConfigureTrack(TrackSettingsChange::Speed(0.75), When::Next),
    )
    .expect("speed");
    assert_eq!(queue.config.action_at_item_end, ActionAtItemEnd::None);
    assert_eq!(queue.config.track.speed(), 0.75);
    let lead = Duration::from_secs_f64(4.0);
    assert_eq!(queue.config.preload_lead, lead);
    let mut mix = kithara_command::Live::<DeckMixSettings, kithara_play::DeckProtocol>::new(
        DeckMixSettings::default(),
    )
    .expect("deck mix owner");
    apply_mix(&mut queue, &mut rig, &mut mix, QueueCommand::SetVolume(0.4)).expect("volume");
    apply_mix(&mut queue, &mut rig, &mut mix, QueueCommand::SetMuted(true)).expect("mute");
    let receipts = rig
        .block(SessionFrame::new(256), 0.0)
        .expect("applied live controls");
    for receipt in receipts {
        assert!(matches!(receipt.outcome(), Outcome::Applied { .. }));
        mix.settle(&receipt).expect("mix owner receipt");
    }
    apply_mix(&mut queue, &mut rig, &mut mix, QueueCommand::Tick).expect("publish applied mix");
    assert_eq!(f32::from(mix.config().volume()), 0.4);
    assert!(mix.config().muted());
    assert_eq!(queue.control().volume(), 0.4);
    assert!(queue.control().is_muted());
}

#[kithara::test(tokio)]
async fn remove_all_items_releases_output_state_and_allows_fresh_playback() {
    let (mut queue, first, _, mut rig, dir) = crate::mock::selected().await;
    assert!(queue.control().is_playing());
    assert!(queue.active_current_index().is_some());
    apply(&mut queue, &mut rig, QueueCommand::RemoveAll).expect("remove all");
    crate::mock::finish(&mut queue, &mut rig);
    assert!(!queue.control().is_playing());
    assert!(queue.active_current_index().is_none());
    assert!(queue.control().current_abr_handle().is_none());
    assert!((queue.control().rate() - 0.0).abs() < f32::EPSILON);
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id: first,
            source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
        },
    )
    .expect("fresh item");
    apply(&mut queue, &mut rig, QueueCommand::Play { at: When::Next }).expect("fresh playback");
    answer_load(&mut queue, &mut rig, &dir).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert!(queue.control().is_playing());
    assert!(queue.active_current_index().is_some());
    apply(&mut queue, &mut rig, QueueCommand::RemoveAll).expect("final clear");
}

#[kithara::test(tokio)]
async fn an_inserted_resource_adopts_the_session_wake_mode() {
    use kithara_render::DispatcherCommand;

    let (mut queue, mut rig, dir) = empty_queue();
    let source = fixture_uri(dir.path(), "inserted.wav");
    crate::mock::seed_fixture(
        queue.config.store.as_ref().expect("fixture store"),
        &source,
        kithara_test_fixtures::assets::fixed_constant_wav_cd_half_4096().bytes(),
    );
    let id = TrackId::allocate();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id,
            source: TrackSource::Uri(source),
        },
    )
    .expect("insert resource through its owner");
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id,
            transition: Transition::None,
        },
    )
    .expect("admit inserted resource");
    rig.opens.drain();
    let mut due = rig
        .opens
        .next_due((), 1)
        .expect("inserted resource load request");
    let DispatcherCommand::Load(request) = due.commands_mut().pop().expect("load command") else {
        panic!("selection must request the inserted source");
    };
    assert!(due.commands_mut().is_empty());
    mock::assert_requested_render_off_bus(
        *request,
        &queue.bus,
        &mock::output(None).get(),
        &pools(),
    )
    .await
    .expect("an admitted source keeps reader events off the audio callback bus");
}

#[kithara::test(tokio)]
async fn close_waits_for_an_admitted_operation() {
    let (mut queue, mut rig, _dir) = empty_queue();
    let id = TrackId::allocate();
    let postbox = queue.postbox.clone();
    let operation = postbox
        .post(QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id,
            source: TrackSource::Uri("memory://admitted".to_owned()),
        })
        .expect("admitted operation");
    let close = postbox.post(QueueCommand::Close).expect("admitted close");
    assert!(operation.seq() < close.seq());
    let mut posts = queue.mailbox.drain();
    let operation = posts.next().expect("operation precedes close");
    let close = posts.next().expect("close follows operation");
    assert!(posts.next().is_none());
    assert!(!queue.shutdown.is_cancelled());
    assert!(matches!(&operation.command, QueueCommand::Append { id: actual, .. } if *actual == id));
    let result = apply(&mut queue, &mut rig, operation.command);
    assert!(result.is_ok());
    operation
        .answer
        .answer(result.map(|_| ()).map_err(Into::into));
    assert_eq!(queue.tracks.records().len(), 1);
    assert!(
        !queue.shutdown.is_cancelled(),
        "the admitted operation completes before close"
    );
    let result = apply(&mut queue, &mut rig, close.command);
    assert!(result.is_ok());
    close.answer.answer(result.map(|_| ()).map_err(Into::into));
    assert!(queue.shutdown.is_cancelled());
    assert!(matches!(
        apply(&mut queue, &mut rig, QueueCommand::Tick),
        Err(PlayError::Closed)
    ));
    assert!(matches!(queue.ensure_open(), Err(PlayError::Closed)));
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn drop_does_not_wait_for_an_admitted_operation() {
    let (mut queue, _, _dir) = empty_queue();
    let observer = queue.shutdown.child();
    let postbox = queue.postbox.clone();
    let _ticket = postbox
        .post(QueueCommand::Tick)
        .expect("admitted operation");
    let operation = queue.mailbox.drain().next().expect("admitted owner post");
    hang_tick!();
    drop(queue);
    hang_reset!();
    assert!(
        postbox.is_closed(),
        "drop closes admission before the outstanding operation is released"
    );
    assert!(
        observer.is_cancelled(),
        "drop cancels the subtree while the admitted operation remains outstanding"
    );
    drop(operation);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn player_lifecycle_admits_only_one_concurrent_close() {
    let (mut queue, _, _, mut rig, _dir) = crate::mock::selected().await;
    {
        let mut port = rig.ring.scope(rig.scope).expect("deck scope");
        for _ in 0..port.available() {
            hang_tick!();
            port.send(
                When::Next,
                kithara_command::Batch {
                    basis: Vec::new(),
                    commands: vec![DeckPart::Mix(kithara_play::DeckMixSettingsChange::Level(
                        1.0,
                    ))],
                },
            )
            .expect("fill deck ring");
            hang_reset!();
        }
        assert_eq!(port.available(), 0);
    }
    assert!(matches!(
        apply(&mut queue, &mut rig, QueueCommand::Close),
        Err(PlayError::Full("deck"))
    ));
    assert!(
        !queue.shutdown.is_cancelled(),
        "refused cleanup reopens admission"
    );
    assert!(queue.ensure_open().is_ok());
    crate::mock::settle(&mut queue, &mut rig, SessionFrame::new(512));
    let postbox = queue.postbox.clone();
    let _first = postbox.post(QueueCommand::Close).expect("first closer");
    let _concurrent = postbox
        .post(QueueCommand::Close)
        .expect("overlapping closer");
    let mut closes = queue.mailbox.drain();
    let first = closes.next().expect("first close");
    assert!(apply(&mut queue, &mut rig, first.command).is_ok());
    let concurrent = closes.next().expect("concurrent close");
    assert!(matches!(
        apply(&mut queue, &mut rig, concurrent.command),
        Err(PlayError::Closed)
    ));
    assert!(closes.next().is_none());
    with_outbox(&mut queue, &mut rig, |queue, out| {
        HostedDeck::close(queue, out)
    })
    .expect("already closed cleanup is idempotent");
    assert!(queue.shutdown.is_cancelled());
}

#[kithara::test(tokio)]
async fn prepare_config_per_track_cancel_is_child_of_player_master() {
    let (queue, _, dir) = empty_queue();
    let prepared = queue
        .config
        .prep
        .as_ref()
        .expect("queue prepare")
        .prepare(
            ResourceConfig::for_src(ResourceSrc::Path(dir.path().join("entry.wav")))
                .store(
                    AssetStore::builder(pools())
                        .backend(StorageBackend::Memory)
                        .build(),
                )
                .build(),
            &mock::output(None).get(),
        )
        .expect("session shape");
    let observer = mock::prepared_resource_context(&prepared)
        .0
        .expect("per-track cancel")
        .child();
    assert!(!observer.is_cancelled());
    drop(queue);
    assert!(
        observer.is_cancelled(),
        "dropping the owner cancels the per-track child via its master"
    );
}

#[kithara::test]
fn prepare_config_preserves_caller_supplied_master(cancel_token: CancelToken) {
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let queue: Queue<TestPools> = Queue::new(
        QueueConfig::builder()
            .prep(prep)
            .cancel(cancel_token.clone())
            .build(),
    );
    let prepared = queue
        .config
        .prep
        .as_ref()
        .expect("queue prepare")
        .prepare(
            ResourceConfig::for_src(ResourceSrc::Path(std::path::PathBuf::from("song.mp3")))
                .store(
                    AssetStore::builder(pools())
                        .backend(StorageBackend::Memory)
                        .build(),
                )
                .build(),
            &mock::output(None).get(),
        )
        .expect("session shape");
    let observer = mock::prepared_resource_context(&prepared)
        .0
        .expect("per-track cancel")
        .child();
    assert!(!observer.is_cancelled());
    cancel_token.cancel();
    assert!(observer.is_cancelled());
}

#[kithara::test(tokio)]
async fn prebound_session_rejects_a_player_built_for_another_sample_rate() {
    let (queue, _, dir) = empty_queue();
    let output =
        kithara_play::SessionOutputView::new(NonZeroU32::new(48_000).expect("session rate"));
    let prepared = queue
        .config
        .prep
        .as_ref()
        .expect("queue prepare")
        .prepare(
            ResourceConfig::for_src(ResourceSrc::Path(dir.path().join("entry.wav")))
                .store(
                    AssetStore::builder(pools())
                        .backend(StorageBackend::Memory)
                        .build(),
                )
                .host_sample_rate(mock::SAMPLE_RATE)
                .build(),
            &output.get(),
        )
        .expect("the session owns the sole output rate");
    assert_eq!(mock::SAMPLE_RATE.get(), 44_100);
    assert_eq!(
        mock::prepared_resource_context(&prepared)
            .1
            .map(NonZeroU32::get),
        Some(48_000)
    );
}
