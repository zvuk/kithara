use super::*;

#[kithara::test]
fn explicit_audio_route_invalidation_restarts_stream_without_backend_error() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    insert(&mut state);
    assert!(matches!(
        state.root_view.sample_rate(),
        SessionSampleRate {
            measured: Some(44_100),
            requested: 44_100,
            ..
        }
    ));
    let slot_node = deck(&state, 0).node;
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        1
    );
    let before_route = host_grid(&state);

    change_route_to(&mut state, "oldDeviceUnavailable");

    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        2,
        "explicit platform route invalidation must restart the audio stream"
    );
    assert_route_boundary(&before_route, &host_grid(&state));
    let first_boundary = host_grid(&state);
    change_route_to(&mut state, "newDeviceAvailable");
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        3,
        "a second physical route invalidation must start a new stream generation"
    );
    assert_route_boundary(&first_boundary, &host_grid(&state));
    assert!(
        state.ctx.is_some(),
        "route invalidation must keep the graph context"
    );
    assert!(
        state
            .ctx
            .as_ref()
            .expect("context")
            .contains_node(deck(&state, 0).node),
        "route invalidation must keep the player graph logically started"
    );
    assert!(
        state
            .ctx
            .as_ref()
            .is_some_and(|ctx| ctx.contains_node(slot_node)),
        "route invalidation must keep the deck's slot node in the graph"
    );
    assert_eq!(deck(&state, 0).node, slot_node);
    assert!(!state.stream_needs_restart);
}

#[kithara::test]
fn unexpected_stream_stop_restarts_stream_without_dropping_the_deck_slot() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    insert(&mut state);
    assert!(state.ctx.is_some());
    assert!(
        state
            .ctx
            .as_ref()
            .expect("context")
            .contains_node(deck(&state, 0).node)
    );
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        1
    );
    let slot_node = deck(&state, 0).node;
    let before_route = host_grid(&state);

    state.stream = None;
    assert!(tick_session(&mut state).is_ok());

    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        2,
        "stream loss must restart the audio stream immediately"
    );
    assert_route_boundary(&before_route, &host_grid(&state));
    assert!(
        state.ctx.is_some(),
        "session must keep the graph context across stream restart"
    );
    assert!(
        state.session_output_node_id.is_some(),
        "session output node id must survive stream restart"
    );
    assert!(
        state
            .ctx
            .as_ref()
            .expect("context")
            .contains_node(deck(&state, 0).node),
        "player graph must remain logically started after stream restart"
    );
    assert!(
        state
            .ctx
            .as_ref()
            .is_some_and(|ctx| ctx.contains_node(slot_node)),
        "the deck's slot node must stay in the graph across stream restart"
    );
    assert_eq!(Some(deck(&state, 0).node), Some(slot_node));
    assert!(!state.stream_needs_restart);
}

#[kithara::test]
fn stream_loss_seen_while_draining_host_commands_restarts_the_stream() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    insert(&mut state);
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        1
    );

    let (_postbox, mut mailbox) = mailbox::<Command, PlayError>();

    state.stream = None;
    let mut posts = OwnerPosts::new();
    state.owner.begin_pass();
    posts.drain(&mut state.owner, &mut mailbox);
    posts.pass(&mut state.owner, true);

    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        2,
        "a stream drop observed during a host command drain must restart the stream"
    );
    assert!(!state.stream_needs_restart);
}

#[kithara::test]
fn empty_host_drain_publishes_a_rendered_transport_commit() {
    let sample_rate = NonZeroU32::new(48_000).expect("test sample rate");
    let mut state = test_state(move |ctx, _| {
        OfflineStream::start(
            ctx,
            BackendConfig::builder()
                .sample_rate(sample_rate)
                .block_frames(NonZeroU32::new(512).expect("block"))
                .declared_latency(Duration::ZERO)
                .build(),
        )
        .map(|stream| SessionStream::Offline(Box::new(stream)))
        .map_err(|error| error.to_string())
    });
    crate::session::state::ensure_ctx(&mut state).expect("active browser graph");
    render_block(&mut state, 0);
    assert_eq!(
        state.root_view.grid().state(),
        BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
    );

    let tempo = Tempo::new(90.0).expect("valid tempo");
    assert!(
        state
            .owner
            .exec(HostSettingsChange::Tempo(tempo), When::Next, &mut ())
            .is_ok()
    );
    state
        .channel
        .as_mut()
        .expect("host channel")
        .publish()
        .expect("an open host channel publishes");
    assert!(tick_session(&mut state).is_ok());
    for clock_samples in [512, 1024, 1536] {
        render_block(&mut state, clock_samples);
    }
    let before = state.root_view.grid();
    let (_postbox, mut mailbox) = mailbox::<Command, PlayError>();

    let mut posts = OwnerPosts::new();
    state.owner.begin_pass();
    posts.drain(&mut state.owner, &mut mailbox);
    posts.pass(&mut state.owner, true);

    assert!(state.root_view.grid().revision() > before.revision());
}

#[kithara::test]
fn failed_stream_restart_is_retried_on_next_tick() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    insert(&mut state);
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        1
    );
    let before_route = host_grid(&state);

    state.stream = None;
    route_loss(|probe| probe.fail_next_start.store(true, Ordering::SeqCst));
    match tick_session(&mut state) {
        Err(err) => assert!(
            matches!(err, SessionError::RestartFailed { .. }),
            "restart failure must be surfaced, got {err:?}"
        ),
        Ok(()) => panic!("failed restart must return an error"),
    }

    assert!(
        state.stream_needs_restart,
        "a failed restart must leave retry state armed"
    );
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        2
    );
    let boundary = host_grid(&state);
    assert_route_boundary(&before_route, &boundary);

    assert!(tick_session(&mut state).is_ok());
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        3,
        "next tick must retry the stream restart"
    );
    let retried = host_grid(&state);
    assert_eq!(retried.stamp(), boundary.stamp());
    assert_eq!(retried.axis(), boundary.axis());
    assert!(!state.stream_needs_restart);
    assert!(
        state
            .ctx
            .as_ref()
            .expect("context")
            .contains_node(deck(&state, 0).node)
    );
}

#[kithara::test]
fn two_restarts_with_a_change_between_them_leave_the_render_copy_on_the_host_settings() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    insert(&mut state);
    configure_sample_rate(&mut state, 48_000);
    let enable = HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(true));
    assert!(matches!(configure_next(&mut state, enable), Ok(())));
    let mut clock = 0;
    render_left(&mut state, &mut clock, 1);

    route_loss(|probe| probe.fail_next_start.store(true, Ordering::SeqCst));
    state.stream = None;
    assert!(matches!(
        tick_session(&mut state),
        Err(SessionError::RestartFailed { .. })
    ));

    let host = *state.settings.config();
    assert_eq!(
        state
            .ctx
            .as_mut()
            .and_then(FirewheelContext::proc_store_mut)
            .and_then(|store| applied_spans(store, 1)?.last())
            .map(|(_, span)| span.settings()),
        Some(host),
        "the restart seeds the render copy with the settings the Host reads"
    );
    assert_eq!(host.sample_rate().get(), 48_000, "the first restart's rate");
    assert!(
        host.metronome().enabled(),
        "the change the first stream applied is settled before the seed"
    );
}
