use kithara_effects::{GainDb, eq::generate_log_spaced_bands};

use super::*;

#[kithara::test(tokio)]
async fn a_running_player_changes_its_eq_layout_in_place() {
    let mut state = test_state(start_route_loss_stream);
    let id = BeatGridId::allocate().expect("fixture deck id");
    assert!(matches!(ask(&mut state, mix_registration(id, 1.0)), Ok(())));
    let directory = constant_fixture();
    let mut clock = 0;
    play_fixture(
        &mut state,
        id,
        &directory.path().join("constant.wav"),
        &mut clock,
        1,
    )
    .await;
    let slots = occupied_slots(&mut state, id);
    assert_eq!(slots.len(), 1);
    let previous_eq = deck_by_grid(&state, id).node;
    let previous_volume = deck_by_grid(&state, id).node;
    let mut layout = generate_log_spaced_bands(4);
    for (band, gain) in layout.iter_mut().zip([-6.0, -3.0, 1.5, 4.0]) {
        band.set_gain_db(GainDb::from(gain));
    }

    fixture_command(
        &mut state,
        id,
        kithara_queue::QueueCommand::SetEqLayout(layout),
    );
    fixture_pass(&mut state, &mut clock);

    let snapshot = deck_snapshot(&mut state, id);
    let mut published_bands = None;
    let mut published_gains = Vec::new();
    state
        .owner
        .with_deck(id, &mut |deck, _, _| {
            let control = deck.control();
            published_bands = Some(control.eq_band_count());
            published_gains = (0..4)
                .map(|band| control.eq_gain(band).expect("configured band"))
                .collect();
        })
        .expect("held deck");
    assert_eq!(published_bands, Some(4));
    assert_eq!(published_gains, vec![-6.0, -3.0, 1.5, 4.0]);
    assert_eq!(
        (0..4)
            .map(|band| snapshot.eq.gain(band).expect("configured band"))
            .collect::<Vec<_>>(),
        [-6.0, -3.0, 1.5, 4.0].map(GainDb::from).to_vec(),
    );
    assert_eq!(occupied_slots(&mut state, id).len(), 1);
    assert_eq!(occupied_slots(&mut state, id)[0], slots[0]);
    assert_eq!(deck_by_grid(&state, id).node, previous_eq);
    assert_eq!(deck_by_grid(&state, id).node, previous_volume);
    assert_eq!(Some(snapshot.eq.bands()), Some(4));
    fixture_command(
        &mut state,
        id,
        kithara_queue::QueueCommand::SetEqGain {
            band: 3,
            gain_db: 5.0,
        },
    );
}

#[kithara::test]
fn host_mix_before_registration_becomes_the_start_level() {
    let runtime = kithara_platform::tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("offline fixture runtime");
    runtime.block_on(kithara_platform::flash::participate(
        async {
            let mut state = test_state(start_route_loss_stream);
            let id = BeatGridId::allocate().expect("fixture deck id");
            assert!(matches!(ask(&mut state, mix_registration(id, 0.4)), Ok(())));
            let directory = constant_fixture();
            let mut clock = 0;
            play_fixture(
                &mut state,
                id,
                &directory.path().join("constant.wav"),
                &mut clock,
                1,
            )
            .await;

            state
                .owner
                .with_deck(id, &mut |_, _, pass| {
                    assert_eq!(pass.mix.level(), 0.4);
                    assert_eq!(pass.mix.gain(), 0.4);
                })
                .expect("held deck");
            let mut output = Vec::new();
            for _ in 0..40 {
                output = fixture_pass(&mut state, &mut clock);
            }
            assert_eq!(
                output.last().copied().expect("rendered sample"),
                0.5 * 0.4,
                "the installed mixer sounds at the configured start gain",
            );
        },
        std::panic::Location::caller(),
    ));
}

#[kithara::test(tokio)]
async fn unexpected_stream_stop_restarts_stream_without_dropping_player_graph_or_future_slots() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    let id = BeatGridId::allocate().expect("fixture deck id");
    assert!(matches!(ask(&mut state, mix_registration(id, 1.0)), Ok(())));
    assert!(state.ctx.is_some());
    assert!(
        state
            .ctx
            .as_ref()
            .expect("context")
            .contains_node(deck_by_grid(&state, id).node)
    );
    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        1
    );
    let directory = constant_fixture();
    let mut clock = 0;
    play_fixture(
        &mut state,
        id,
        &directory.path().join("constant.wav"),
        &mut clock,
        1,
    )
    .await;
    let slots = occupied_slots(&mut state, id);
    assert_eq!(slots.len(), 1);
    let node = deck_by_grid(&state, id).node;
    let before_route = host_grid(&state);

    state.stream = None;
    assert!(tick_session(&mut state).is_ok());

    assert_eq!(
        route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
        2
    );
    assert_route_boundary(&before_route, &host_grid(&state));
    assert!(state.ctx.is_some());
    assert!(state.session_output_node_id.is_some());
    assert!(
        state
            .ctx
            .as_ref()
            .expect("context")
            .contains_node(deck_by_grid(&state, id).node)
    );
    assert!(
        state
            .ctx
            .as_ref()
            .is_some_and(|ctx| ctx.contains_node(node))
    );
    assert_eq!(deck_by_grid(&state, id).node, node);
    assert_eq!(occupied_slots(&mut state, id), slots);

    clock = 0;
    play_fixture(
        &mut state,
        id,
        &directory.path().join("constant.wav"),
        &mut clock,
        2,
    )
    .await;
    assert_eq!(occupied_slots(&mut state, id).len(), 2);
    assert!(!state.stream_needs_restart);
}
