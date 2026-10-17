use super::*;

#[derive(Clone, Copy)]
enum IdleOperation {
    Stop,
    AllocateSlot,
    ReleaseSlot,
}

#[kithara::test]
#[case(IdleOperation::Stop)]
#[case(IdleOperation::AllocateSlot)]
#[case(IdleOperation::ReleaseSlot)]
fn engine_not_running_operations_return_error(#[case] operation: IdleOperation) {
    let mut state = test_state(start_route_loss_stream);
    let id = BeatGridId::allocate().expect("unregistered deck identity");
    let root = host_grid(&state);
    let scopes = state.channel_config.values().scopes;
    let mut visited = false;
    let result = match operation {
        IdleOperation::Stop => state.owner.apply(HostCommand::Close(id)).map(|_| ()),
        IdleOperation::AllocateSlot => state.owner.with_deck(id, &mut |deck, out, _| {
            visited = true;
            let config = kithara_play::ResourceConfig::for_src(
                kithara_play::ResourceSrc::parse("https://example.com/unregistered.wav")
                    .expect("fixture resource"),
            )
            .store(kithara_assets::AssetStore::builder(pools()).build())
            .build();
            kithara_play::Player::apply(
                deck,
                kithara_queue::QueueCommand::Append {
                    position: kithara_play::Position::ZERO,
                    id: kithara_events::TrackId::allocate(),
                    source: kithara_queue::TrackSource::Config(Box::new(config)),
                },
                out,
            )
            .expect("the absent deck must never be borrowed");
        }),
        IdleOperation::ReleaseSlot => state.owner.apply(HostCommand::Release(id)).map(|_| ()),
    };

    assert!(matches!(
        result,
        Err(PlayError::Session(SessionError::DeckNotFound(refused))) if refused == id
    ));
    assert!(!visited);
    assert_eq!(host_grid(&state).stamp(), root.stamp());
    assert_eq!(host_grid(&state).axis(), root.axis());
    assert_eq!(state.channel_config.values().scopes, scopes);
    assert!(state.root_view.is_empty());
    assert_eq!(deck_count(&state), 0);
    assert!(state.ctx.is_none());
    assert!(state.stream.is_none());
    assert!(state.channel.is_none());
    assert!(state.owner.clock().is_none());
    state
        .owner
        .each_deck(&mut |_, _, _, _| panic!("no deck was registered"));
}

#[kithara::test]
fn registration_rejects_a_player_before_canonical_attachment() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    let id = BeatGridId::allocate().expect("fixture deck identity");
    let root = state.root.id();
    assert!(state.root_view.is_empty());
    assert_eq!(deck_count(&state), 0);

    assert!(matches!(ask(&mut state, registration(id)), Ok(())));
    assert_eq!(state.root.id(), root);
    assert!(state.root_view.holds(id));
    assert_eq!(deck_count(&state), 1);
    assert_eq!(deck_by_grid(&state, id).id, id);
    assert!(state.stream.is_some());
    let stamp = host_grid(&state).stamp();
    let scopes = state.channel_config.values().scopes;

    assert!(matches!(
        ask(&mut state, registration(id)),
        Err(PlayError::Session(SessionError::DeckAttached(refused))) if refused == id
    ));
    assert_eq!(host_grid(&state).stamp(), stamp);
    assert_eq!(state.channel_config.values().scopes, scopes);
    assert_eq!(deck_count(&state), 1);
    assert!(state.root_view.holds(id));

    remove(&mut state, id);
    assert_eq!(state.root.id(), root);
    assert!(state.root_view.is_empty());
    assert_eq!(deck_count(&state), 0);
}

#[kithara::test]
fn detach_is_rejected_while_the_graph_projection_is_live() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    let id = insert(&mut state);
    let root = state.root.id();
    let grid = host_grid(&state);
    let node = deck_by_grid(&state, id).node;
    let foreign = BeatGridId::allocate().expect("foreign deck identity");

    assert!(matches!(
        ask(&mut state, HostCommand::Close(foreign)),
        Err(PlayError::Session(SessionError::DeckNotFound(refused))) if refused == foreign
    ));
    assert_eq!(host_grid(&state).stamp(), grid.stamp());
    assert_eq!(host_grid(&state).axis(), grid.axis());
    assert!(state.root_view.holds(id));
    assert_eq!(deck_count(&state), 1);
    assert_eq!(deck_by_grid(&state, id).node, node);
    assert!(state.ctx.as_ref().expect("live output").contains_node(node));

    assert!(matches!(ask(&mut state, HostCommand::Close(id)), Ok(())));
    assert_eq!(state.root.id(), root);
    assert!(state.root_view.is_empty());
    assert_eq!(deck_count(&state), 0);
    assert!(state.ctx.is_none());
    assert!(state.stream.is_none());
    assert!(matches!(
        ask(&mut state, HostCommand::Close(id)),
        Err(PlayError::Session(SessionError::DeckNotFound(refused))) if refused == id
    ));
    assert!(state.root_view.is_empty());
    assert_eq!(deck_count(&state), 0);
}
