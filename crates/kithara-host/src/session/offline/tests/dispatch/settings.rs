use super::*;

#[kithara::test]
fn each_tap_takes_one_group_and_idle_teardown_clears_both() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    let id = insert(&mut state);

    let drops = Arc::new(AtomicU64::new(0));
    let mut outputs = OutputGroup::new();
    outputs.push(mix_tap_writer(&drops));
    outputs.push(mix_tap_writer(&drops));
    assert!(matches!(
        ask(
            &mut state,
            HostCommand::AttachOutputs {
                tap: Tap::Master,
                outputs,
            }
        ),
        Ok(())
    ));
    assert!(
        matches!(state.taps.slot(Tap::Master), Some(TapSlot::Installed(_))),
        "a tap armed on a running session reaches the graph at once"
    );

    let mut second = OutputGroup::new();
    second.push(mix_tap_writer(&drops));
    assert!(
        matches!(
            ask(
                &mut state,
                HostCommand::AttachOutputs {
                    tap: Tap::Master,
                    outputs: second,
                }
            ),
            Err(PlayError::Session(SessionError::TapActive))
        ),
        "a second consumer must be rejected instead of silently replacing the first"
    );

    let mut beside = OutputGroup::new();
    beside.push(mix_tap_writer(&drops));
    assert!(
        matches!(
            ask(
                &mut state,
                HostCommand::AttachOutputs {
                    tap: Tap::Output,
                    outputs: beside,
                }
            ),
            Ok(())
        ),
        "the output tap takes its own group beside the master tap"
    );

    remove(&mut state, id);
    assert!(state.session_limiter_node_id.is_none());
    assert!(
        state.taps.slot(Tap::Master).is_none() && state.taps.slot(Tap::Output).is_none(),
        "idle teardown must clear both taps with the context they lived in"
    );
}

#[kithara::test]
fn a_metronome_change_in_flight_at_an_idle_teardown_sounds_in_the_next_stream() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    let id = insert(&mut state);
    assert!(matches!(
        ask(
            &mut state,
            HostCommand::Configure(
                HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(true)),
                When::Next
            )
        ),
        Ok(())
    ));
    assert!(
        !state.settings.config().metronome().enabled(),
        "the change waits for a block that never renders"
    );

    remove(&mut state, id);
    assert!(
        state.settings.config().metronome().enabled(),
        "the teardown folds the change in flight into the settings"
    );

    insert(&mut state);
    let block = render_block(&mut state, 0);
    assert!(
        block.iter().any(|sample| sample.abs() > 0.1),
        "the first block of the next stream clicks session beat 0"
    );
}

#[kithara::test]
fn changes_past_the_queue_capacity_flow_while_blocks_render_without_a_tick() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    insert(&mut state);
    let mut clock = 0;
    let mut applied = state.settings.config().metronome().level();
    for step in 0..2 * host_queue_capacity() {
        let (level, change) = level_change(step);
        assert!(
            matches!(configure_next(&mut state, change), Ok(())),
            "change {step} goes out"
        );
        assert_eq!(
            state.settings.config().metronome().level(),
            applied,
            "the Host reads the change the last block applied"
        );
        render_left(&mut state, &mut clock, 1);
        applied = level;
    }
}

#[kithara::test]
fn a_full_queue_refuses_a_change_until_a_block_answers_the_ones_in_flight() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    insert(&mut state);
    let capacity = host_queue_capacity();
    for step in 0..capacity {
        assert!(
            matches!(configure_next(&mut state, level_change(step).1), Ok(())),
            "change {step} fits the queue"
        );
    }
    let before = *state.settings.config();

    assert!(matches!(
        configure_next(&mut state, level_change(capacity).1),
        Err(PlayError::Session(SessionError::HostQueueFull))
    ));
    assert_eq!(
        *state.settings.config(),
        before,
        "a refused change leaves the settings alone"
    );
    assert_eq!(
        state.settings.pending().count(),
        usize::from(capacity),
        "a refused change leaves the changes in flight alone"
    );

    let mut clock = 0;
    render_left(&mut state, &mut clock, 1);
    assert!(matches!(
        configure_next(&mut state, level_change(capacity).1),
        Ok(())
    ));
    assert_eq!(
        state.settings.config().metronome().level(),
        level_change(capacity - 1).0,
        "the block applied every change in flight"
    );
}

#[kithara::test]
fn a_ducking_change_lowers_a_sounding_dc_along_a_ramp() {
    const DC: f32 = 0.25;
    /// The share of the session output `Hard` ducking leaves: 28 dB down.
    const HARD_DUCKED: f32 = 0.04;
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    insert(&mut state);
    let session_output = state
        .session_output_node_id
        .expect("the session output runs");
    let ctx = state.ctx.as_mut().expect("the context runs");
    let dc = add_graph_node(ctx, DcNode(DC)).expect("the graph takes the source");
    ctx.connect(dc, session_output, &[(0, 0), (1, 1)], false)
        .expect("the source feeds the session output");
    ctx.update().expect("the graph takes the source in");
    let mut clock = 0;
    let before = render_left(&mut state, &mut clock, 8);
    let undiminished = *before.last().expect("the stream rendered");
    assert!(
        (undiminished - DC).abs() < 1e-4,
        "the DC reaches the output whole before the change: {undiminished}"
    );

    assert!(matches!(
        ask(
            &mut state,
            HostCommand::Configure(
                HostSettingsChange::Ducking(SessionDuckingMode::Hard),
                When::Next
            )
        ),
        Ok(())
    ));
    let after = render_left(&mut state, &mut clock, 40);

    let ducked = DC * HARD_DUCKED;
    let settled = *after.last().expect("the stream rendered");
    assert!(
        (settled - ducked).abs() < 1e-4,
        "the DC settles at the hard ducking: {settled}, not {ducked}"
    );
    let steepest = [undiminished]
        .iter()
        .chain(&after)
        .zip(&after)
        .map(|(previous, sample)| (sample - previous).abs())
        .fold(0.0, f32::max);
    assert!(
        steepest < (DC - ducked) / 100.0,
        "no step between neighbouring samples on the way down: {steepest}"
    );
}

#[kithara::test]
fn a_deck_attached_after_an_idle_teardown_leaves_through_the_next_one() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    let first = insert(&mut state);
    remove(&mut state, first);

    let second = insert(&mut state);
    match ask(&mut state, HostCommand::Close(second)) {
        Ok(()) => {}
        Err(error) => {
            panic!("a deck that joined after a route boundary must follow the next: {error}")
        }
    }
}

#[kithara::test]
fn session_output_has_exactly_one_limiter_rebuilt_on_route_recreate() {
    route_loss(RouteLossProbe::reset);

    let mut state = test_state(start_route_loss_stream);
    let id = insert(&mut state);
    assert!(
        state.session_limiter_node_id.is_some(),
        "limiter node exists after start"
    );

    remove(&mut state, id);
    assert!(state.session_limiter_node_id.is_none());
    assert!(state.session_output_node_id.is_none());

    insert(&mut state);
    assert!(
        state.session_limiter_node_id.is_some(),
        "route recreate rebuilds the limiter node"
    );
}
