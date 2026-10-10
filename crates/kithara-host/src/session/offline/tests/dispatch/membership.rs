use super::*;

#[kithara::test]
fn an_attached_deck_runs_until_it_is_detached() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    let host_id = state.root.id();

    let grid_id = insert(&mut state);
    assert!(state.deck_nodes.iter().any(|deck| deck.id == grid_id));
    assert!(state.root_view.holds(grid_id));
    assert!(
        state
            .ctx
            .as_ref()
            .expect("context")
            .contains_node(deck_by_grid(&state, grid_id).node)
    );

    remove(&mut state, grid_id);

    assert_eq!(state.root.id(), host_id);
    assert!(!state.deck_nodes.iter().any(|deck| deck.id == grid_id));
    assert!(!state.root_view.holds(grid_id));
    assert_eq!(deck_count(&state), 0);
}

#[kithara::test]
fn attach_refuses_an_identity_the_session_already_holds() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    let grid_id = insert(&mut state);
    let scopes = state.channel_config.values().scopes;

    assert!(matches!(
        ask(&mut state, registration(grid_id)),
        Err(PlayError::Session(SessionError::DeckAttached(refused)))
            if refused == grid_id
    ));
    assert_eq!(state.channel_config.values().scopes, scopes);
    assert!(state.root_view.holds(grid_id));
    assert_eq!(deck_count(&state), 1);
}

#[kithara::test]
fn detach_refuses_a_deck_the_session_does_not_hold() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    let held = insert(&mut state);
    let grid_id = BeatGridId::allocate().expect("fixture foreign grid id");

    assert!(matches!(
        ask(&mut state, HostCommand::Close(grid_id)),
        Err(PlayError::Session(SessionError::DeckNotFound(refused)))
            if refused == grid_id
    ));
    assert!(state.root_view.holds(held));
}

#[kithara::test]
fn root_view_publishes_the_decks_the_session_holds() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    assert!(state.root_view.is_empty());
    let grid_id = insert(&mut state);

    assert!(state.root_view.holds(grid_id));
    assert!(!state.root_view.is_empty());

    remove(&mut state, grid_id);
    assert!(!state.root_view.holds(grid_id));
    assert!(state.root_view.is_empty());
}

#[kithara::test]
fn exhausted_player_identity_refuses_the_deck_whole() {
    let mut state = test_state(start_route_loss_stream);
    let grid_id = BeatGridId::allocate().expect("fixture player grid id");
    state.channel_config = kithara_command::ScopedConfig::builder()
        .scope(kithara_command::ChannelConfig::builder().targets(1).build())
        .build();

    let reply = ask(&mut state, registration(grid_id));

    assert!(matches!(
        reply,
        Err(PlayError::Internal(reason)) if reason == kithara_command::OpenError::Targets {
            targets: kithara_play::DeckMixerConfig::default().slots().get(),
            limit: 1,
        }.to_string()
    ));
    assert_eq!(state.channel_config.values().scope.values().targets, 1);
    assert_eq!(deck_count(&state), 0);
    assert!(!state.deck_nodes.iter().any(|deck| deck.id == grid_id));
    assert!(!state.root_view.holds(grid_id));
    assert!(state.reserved_session_grid.is_some());
}

#[kithara::test]
fn the_published_sample_rate_separates_the_measured_stream_from_the_request() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    let before = state.root_view.sample_rate();
    assert_eq!(
        before.measured, None,
        "a session with no stream has measured nothing"
    );
    assert_eq!(
        before.output(),
        TestState::DEFAULT_SAMPLE_RATE,
        "until a stream exists the resampler is built for the requested rate"
    );

    configure_sample_rate(&mut state, 48_000);
    assert!(matches!(
        state.root_view.sample_rate(),
        SessionSampleRate {
            measured: None,
            requested: 48_000,
            ..
        }
    ));
    insert(&mut state);
    assert!(matches!(
        state.root_view.sample_rate(),
        SessionSampleRate {
            measured: Some(48_000),
            requested: 48_000,
            ..
        }
    ));
}

#[kithara::test]
fn a_sample_rate_set_while_idle_is_the_rate_play_starts_the_stream_at() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    let rate = NonZeroU32::new(48_000).expect("48000 is not zero");
    configure_sample_rate(&mut state, rate.get());
    insert(&mut state);

    let started = state
        .ctx
        .as_ref()
        .and_then(FirewheelContext::stream_info)
        .expect("play starts the stream")
        .sample_rate;
    assert_eq!(
        started, rate,
        "the stream starts at the rate set while idle"
    );
}

#[kithara::test]
fn the_published_stream_shape_prefers_measurement_over_an_explicit_request() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    assert_eq!(state.root_view.output.get().stream_shape, None);

    state.requested_max_block_frames = NonZeroU32::new(128);
    state.publish_root();
    let requested = state
        .root_view
        .output
        .get()
        .stream_shape
        .expect("the explicit output block is published before stream start");
    assert_eq!(requested.max_block_frames.get(), 128);
    assert_eq!(requested.sample_rate.get(), TestState::DEFAULT_SAMPLE_RATE);

    let player_id = insert(&mut state);
    let measured = state
        .root_view
        .output
        .get()
        .stream_shape
        .expect("the running stream publishes its measured output shape");
    assert_eq!(measured.max_block_frames.get(), 512);
    assert_eq!(measured.sample_rate.get(), TestState::DEFAULT_SAMPLE_RATE);
    configure_sample_rate(&mut state, 48_000);
    assert_eq!(
        state
            .root_view
            .output
            .get()
            .stream_shape
            .expect("published shape")
            .sample_rate
            .get(),
        48_000
    );
    remove(&mut state, player_id);
    let stopped = state
        .root_view
        .output
        .get()
        .stream_shape
        .expect("configured shape after stop");
    assert_eq!(stopped.max_block_frames.get(), 128);
    assert_eq!(stopped.sample_rate.get(), 48_000);
}

#[kithara::test]
fn a_deck_whose_buffers_outgrow_the_measured_block_is_refused_whole() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    state.requested_max_block_frames = NonZeroU32::new(128);
    let grid_id = BeatGridId::allocate().expect("fixture player grid id");
    let registration =
        configured_registration(grid_id, NonZeroUsize::new(64), NonZeroUsize::new(441));

    assert!(matches!(
        ask(&mut state, registration),
        Err(PlayError::Session(SessionError::BufferGeometry(
            BufferGeometryError::BudgetExceeded {
                max_block_frames: 512,
                render_quantum_frames: 64,
                required_frames: 639,
                budget_frames: 441,
            }
        )))
    ));
    assert_eq!(deck_count(&state), 0);
    assert!(!state.root_view.holds(grid_id));
    assert!(
        state.ctx.is_none(),
        "a refused deck leaves no output running"
    );
}
