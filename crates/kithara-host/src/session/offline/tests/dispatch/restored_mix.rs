use kithara_play::DeckMixSettingsChange;

use super::*;

fn levels(state: &mut TestState) -> Vec<f32> {
    let mut values = Vec::new();
    state
        .owner
        .each_deck(&mut |_, _, _, pass| values.push(pass.mix.level()));
    values
}

fn apply_levels(
    state: &mut TestState,
    ids: &[BeatGridId],
    changes: &[(usize, f32)],
    clock: &mut u64,
) {
    state.owner.begin_pass();
    for &(index, level) in changes {
        state
            .owner
            .with_deck(ids[index], &mut |_, out, _| {
                assert!(
                    out.mix(When::Next, DeckMixSettingsChange::Level(level))
                        .is_ok()
                );
            })
            .expect("held deck");
    }
    state.owner.pass();
    render_left(state, clock, 1);
    state.owner.begin_pass();
    state.owner.pass();
}

#[kithara::test]
fn host_mix_updates_one_two_and_four_players_together() {
    let mut state = test_state(start_route_loss_stream);
    state.channel_config = kithara_command::ScopedConfig::builder()
        .scopes(std::num::NonZeroU16::new(4).expect("four decks"))
        .scope(state.channel_config.values().scope)
        .build();
    let ids = (0..4).map(|_| insert(&mut state)).collect::<Vec<_>>();
    let mut clock = 0;

    apply_levels(&mut state, &ids, &[(0, 0.1)], &mut clock);
    assert_eq!(levels(&mut state)[0], 0.1);

    apply_levels(&mut state, &ids, &[(1, 0.2), (2, 0.3)], &mut clock);
    let two = levels(&mut state);
    assert_eq!(two[1], 0.2);
    assert_eq!(two[2], 0.3);
    assert_eq!(two[3], 1.0);

    apply_levels(
        &mut state,
        &ids,
        &[(0, 0.4), (1, 0.5), (2, 0.6), (3, 0.7)],
        &mut clock,
    );
    let four = levels(&mut state);
    assert_eq!(four[0], 0.4);
    assert_eq!(four[3], 0.7);
}
