use kithara_play::HostedDeck;

use super::*;

#[kithara::test]
fn a_ducking_mode_from_a_player_session_is_the_host_setting() {
    let mut state = test_state(start_route_loss_stream);

    assert!(matches!(
        ask(
            &mut state,
            HostCommand::Configure(
                HostSettingsChange::Ducking(SessionDuckingMode::Soft),
                When::Next,
            ),
        ),
        Ok(())
    ));
    assert_eq!(state.settings.config().ducking(), SessionDuckingMode::Soft);
}

#[kithara::test]
fn duplicate_graph_projection_is_rejected() {
    let mut state = test_state(start_route_loss_stream);
    let grid_id = BeatGridId::allocate().expect("fixture deck id");
    state.channel_config = kithara_command::ScopedConfig::builder()
        .scopes(std::num::NonZeroU16::new(2).expect("two scopes"))
        .scope(state.channel_config.values().scope)
        .build();

    assert!(matches!(ask(&mut state, registration(grid_id)), Ok(())));
    assert!(matches!(
        ask(&mut state, registration(grid_id)),
        Err(PlayError::Session(SessionError::DeckAttached(id))) if id == grid_id
    ));

    let channel = state.channel.as_mut().expect("running channel");
    let next = channel.open(1).expect("the duplicate consumed no scope");
    assert_eq!(next.index(), 1);
    assert_eq!(next.generation(), 0);
    channel
        .close(next)
        .expect("retire the unused fixture scope");
    let mut held = Vec::new();
    state.owner.each_deck(&mut |id, _, _, _| held.push(id));
    assert_eq!(held, vec![grid_id]);
    assert_eq!(deck_count(&state), 1);
}

#[kithara::test]
fn engine_start_stop_roundtrip() {
    let mut state = test_state(start_route_loss_stream);
    let id = BeatGridId::allocate().expect("fixture deck id");

    assert!(matches!(ask(&mut state, registration(id)), Ok(())));
    assert!(state.stream.is_some());
    assert!(
        state
            .ctx
            .as_ref()
            .and_then(FirewheelContext::stream_info)
            .is_some()
    );
    assert_eq!(render_block(&mut state, 0).len(), 1024);

    assert!(matches!(ask(&mut state, HostCommand::Close(id)), Ok(())));
    assert!(state.stream.is_none());
    assert!(state.ctx.is_none());
}

#[kithara::test]
fn measured_output_block_rejects_player_before_graph_start() {
    let mut state = test_state(start_route_loss_stream);
    state.requested_max_block_frames = NonZeroU32::new(128);
    let id = BeatGridId::allocate().expect("fixture deck id");
    let command = configured_registration(id, NonZeroUsize::new(64), NonZeroUsize::new(441));

    assert!(matches!(
        ask(&mut state, command),
        Err(PlayError::Session(SessionError::BufferGeometry(
            BufferGeometryError::BudgetExceeded {
                max_block_frames: 512,
                render_quantum_frames: 64,
                required_frames: 639,
                budget_frames: 441,
            }
        )))
    ));
    assert!(!state.root_view.holds(id));
    assert!(!state.deck_nodes.iter().any(|node| node.id == id));
    assert!(state.stream.is_none());
}

#[kithara::test]
fn sample_rate_query_separates_the_measured_stream_from_the_request() {
    let mut state = test_state(start_route_loss_stream);
    let before = state.root_view.sample_rate();
    assert_eq!(before.measured, None);
    assert_eq!(before.output(), 44_100);

    let id = BeatGridId::allocate().expect("fixture deck id");
    assert!(matches!(ask(&mut state, registration(id)), Ok(())));
    assert!(matches!(
        state.root_view.sample_rate(),
        SessionSampleRate {
            measured: Some(44_100),
            requested: 44_100,
            ..
        }
    ));
    configure_sample_rate(&mut state, 48_000);
    assert!(matches!(
        state.root_view.sample_rate(),
        SessionSampleRate {
            measured: Some(48_000),
            requested: 48_000,
            ..
        }
    ));
    state.owner.begin_pass();
    state.owner.pass();
    assert!(matches!(
        state.root_view.sample_rate(),
        SessionSampleRate {
            measured: Some(48_000),
            requested: 48_000,
            ..
        }
    ));

    let mut state = test_state(|context, _requested| start_route_loss_stream(context, 48_000));
    assert_eq!(state.root_view.sample_rate().requested, 44_100);
    assert_eq!(state.root_view.sample_rate().measured, None);
    assert!(state.stream.is_none());
    let id = insert(&mut state);
    assert_eq!(state.root_view.sample_rate().requested, 44_100);
    assert_eq!(state.root_view.sample_rate().measured, Some(48_000));
    assert_eq!(state.root_view.sample_rate().output(), 48_000);
    state
        .owner
        .with_deck(id, &mut |deck, _, _| {
            assert_eq!(deck.mixer_config().sample_rate().get(), 44_100);
        })
        .expect("held deck retains immutable construction config");
    fixture_pass(&mut state, &mut 0);
    assert_eq!(deck_snapshot(&mut state, id).sample_rate, 48_000);
    assert_eq!(state.root_view.sample_rate().requested, 44_100);
    assert_eq!(state.root_view.sample_rate().measured, Some(48_000));
}

#[kithara::test]
fn pending_session_binds_once() {
    use crate::session::tests::deck_probe::{Seen, probe, so_far, ticks};

    let (mut owner, messages) = probe_owner(start_route_loss_stream);
    let (id, deck, seen) = probe();
    assert!(so_far(&seen).is_empty());
    assert!(matches!(
        owner.with_deck(id, &mut |deck, out, pass| deck.tick(pass, out)),
        Err(PlayError::Session(SessionError::DeckNotFound(missing))) if missing == id
    ));

    owner.register(id, deck).expect("bind canonical owner");
    let held = so_far(&seen);
    let [Seen::Held(waker), Seen::Drained(_)] = held.as_slice() else {
        panic!("binding holds the wake route once and drains the deck")
    };
    let route = waker.clone();
    assert!(matches!(
        owner.with_deck(id, &mut |deck, out, pass| deck.tick(pass, out)),
        Ok(())
    ));
    assert_eq!(ticks(&so_far(&seen)), 1);
    let (_, duplicate, duplicate_seen) = probe();
    assert!(matches!(
        owner.register(id, duplicate),
        Err(PlayError::Session(SessionError::DeckAttached(bound))) if bound == id
    ));
    assert!(matches!(
        so_far(&duplicate_seen).as_slice(),
        [Seen::Dropped]
    ));
    assert!(so_far(&seen).is_empty());
    route.wake_by_ref();
    let posts = messages.try_iter().collect::<Vec<_>>();
    assert_eq!(posts.len(), 1);
    for message in posts {
        message.run(&mut owner);
    }
    assert!(matches!(so_far(&seen).as_slice(), [Seen::Drained(_)]));
}

#[kithara::test]
fn the_requested_rate_never_reaches_the_session() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    configure_sample_rate(&mut state, 48_000);
    assert_eq!(state.root_view.sample_rate().requested, 48_000);
    assert_eq!(state.messages.try_iter().count(), 0);
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        0
    );

    insert(&mut state);
    let starts = route_loss(|probe| probe.start_count.load(Ordering::SeqCst));
    assert_eq!(starts, 1);
    assert_eq!(state.root_view.sample_rate().requested, 48_000);
    assert_eq!(state.messages.try_iter().count(), 0);
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        starts
    );
}
