use kithara_effects::GainDb;
use kithara_play::{
    CrossfaderGain, DeckEqChange, DeckMixSettings, DeckMixSettingsChange, DeckMixerConfig,
    HostedDeck,
};
use kithara_test_utils::off_thread::OffThread;

use super::*;

fn mix_level(state: &mut TestState, id: BeatGridId) -> f32 {
    let mut level = None;
    state
        .owner
        .with_deck(id, &mut |_, _, pass| {
            level = Some(f32::from(pass.mix.crossfader()));
        })
        .expect("held deck");
    level.expect("owner lent the confirmed mix")
}

fn apply_level(
    state: &mut TestState,
    id: BeatGridId,
    level: f32,
) -> Result<kithara_command::Seq, PlayError> {
    let level = CrossfaderGain::checked(level).expect("fixture levels are crossfader gains");
    let mut result = Err(PlayError::NotReady);
    state.owner.with_deck(id, &mut |_, out, _| {
        result = out.mix(When::Next, DeckMixSettingsChange::Crossfader(level));
    })?;
    result
}

fn settle_block(state: &mut TestState, clock: &mut u64) -> Vec<crate::HostSettled> {
    state.owner.pass();
    render_left(state, clock, 1);
    state.owner.begin_pass();
    state.owner.pass()
}

fn assert_applied<const N: usize>(
    receipts: Vec<crate::HostSettled>,
    expected: [kithara_command::Seq; N],
) {
    let applied = receipts
        .into_iter()
        .filter_map(|receipt| match receipt {
            crate::HostSettled::Batch { seq, outcome } => {
                Some((seq, outcome.expect("each accepted batch must apply")))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        applied.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
        expected
    );
    assert!(applied.windows(2).all(|pair| pair[0].1 == pair[1].1));
}

#[kithara::test]
fn an_idle_host_has_no_output_or_registered_decks() {
    let mut state = test_state(start_route_loss_stream);
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let deck = Queue::<TestPools>::new(QueueConfig::builder().prep(prep).build());
    let mut held = Vec::new();
    state.owner.each_deck(&mut |id, _, _, _| held.push(id));
    assert!(state.stream.is_none());
    assert_eq!(deck.mixer_config().slots().get(), 4);
    assert!(held.is_empty());
    assert_eq!(state.deck_nodes.len(), 0);
}

#[kithara::test(tokio)]
async fn a_default_deck_renders_at_unity_gain() {
    let owner = OffThread::spawn("host-default-mix", || {
        Ok::<_, ()>((test_state(start_route_loss_stream), 0_u64))
    })
    .await
    .expect("fixture owner starts");
    let id = owner
        .call(|(state, _)| {
            let prep = ResourcePrep::builder()
                .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
                .block_on_underrun(true)
                .build();
            let deck = Queue::new(QueueConfig::builder().prep(prep).build());
            assert_eq!(
                deck.mixer_config().mix().crossfader(),
                CrossfaderGain::DEFAULT
            );
            assert!(state.stream.is_none());
            let id = BeatGridId::allocate().expect("fixture deck id");
            ask(
                state,
                HostCommand::Register {
                    id,
                    deck: Box::new(deck),
                },
            )
            .expect("default deck registration");
            id
        })
        .await;
    let directory = owner.call(|_| constant_fixture()).await;
    play_fixture(&owner, id, &directory.path().join("constant.wav"), 1).await;
    owner
        .call(move |(state, clock)| {
            let mut output = Vec::new();
            for _ in 0..40 {
                output = fixture_pass(state, clock);
            }
            assert_eq!(mix_level(state, id), 1.0);
            assert_eq!(output.len(), 1024);
            assert!(output.iter().all(|sample| *sample == 0.5));
        })
        .await;
    owner.close().await;
}

#[kithara::test]
fn independent_deck_levels_publish_only_after_rendering() {
    let mut state = test_state(start_route_loss_stream);
    let first = insert(&mut state);
    let second = insert(&mut state);
    state.owner.begin_pass();
    let first_seq = apply_level(&mut state, first, 0.25).expect("first level");
    let second_seq = apply_level(&mut state, second, 0.75).expect("second level");
    assert_eq!(mix_level(&mut state, first), 1.0);
    assert_eq!(mix_level(&mut state, second), 1.0);
    assert_applied(settle_block(&mut state, &mut 0), [first_seq, second_seq]);
    assert_eq!(mix_level(&mut state, first), 0.25);
    assert_eq!(mix_level(&mut state, second), 0.75);
}

#[kithara::test]
fn registration_adopts_the_decks_configured_level() {
    let mut state = test_state(start_route_loss_stream);
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let deck = Queue::new(
        QueueConfig::builder()
            .prep(prep)
            .mixer(
                DeckMixerConfig::builder()
                    .mix(
                        DeckMixSettings::builder()
                            .crossfader(CrossfaderGain::checked(0.2).expect("a crossfader gain"))
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    assert_eq!(f32::from(deck.mixer_config().mix().crossfader()), 0.2);
    let id = BeatGridId::allocate().expect("fixture deck id");
    ask(
        &mut state,
        HostCommand::Register {
            id,
            deck: Box::new(deck),
        },
    )
    .expect("first registration");
    assert_eq!(
        mix_level(&mut state, id),
        0.2,
        "registration adopts the configured level"
    );
}

#[kithara::test]
fn a_confirmed_deck_level_survives_output_restart() {
    let mut state = test_state(start_route_loss_stream);
    let id = insert(&mut state);
    state.owner.begin_pass();
    apply_level(&mut state, id, 0.4).expect("deck level");
    settle_block(&mut state, &mut 0);
    state.stream = None;
    assert_eq!(mix_level(&mut state, id), 0.4);
    ask(&mut state, HostCommand::Restart).expect("restart output");
    assert_eq!(mix_level(&mut state, id), 0.4);
}

#[kithara::test]
fn an_empty_group_sends_no_batch() {
    let mut state = test_state(start_route_loss_stream);
    let id = insert(&mut state);
    state
        .owner
        .with_deck(id, &mut |_, out, _| {
            let (_, sent) = out.together(When::Next, |_| Ok(())).expect("empty batch");
            assert_eq!(sent, None);
        })
        .expect("held deck");
    assert_applied(settle_block(&mut state, &mut 0), []);
    assert_eq!(mix_level(&mut state, id), 1.0);
}

#[kithara::test]
fn independent_levels_for_one_deck_apply_in_send_order() {
    let mut state = test_state(start_route_loss_stream);
    let id = insert(&mut state);
    state.owner.begin_pass();
    let first = apply_level(&mut state, id, 0.3).expect("first independent level");
    let second = apply_level(&mut state, id, 0.4).expect("second independent level");
    assert!(second > first);
    assert_eq!(mix_level(&mut state, id), 1.0);
    assert_applied(settle_block(&mut state, &mut 0), [first, second]);
    assert_eq!(mix_level(&mut state, id), 0.4);
}

#[kithara::test]
fn a_refused_group_sends_none_of_its_staged_eq_changes() {
    fn stage(out: &mut kithara_play::Outbox<'_, TestPools>) -> Result<(), PlayError> {
        for (band, gain) in [(0, -6.0), (1, 3.0)] {
            assert_eq!(
                out.eq(vec![DeckEqChange::Gain {
                    band,
                    gain: GainDb::from(gain),
                }])?,
                None,
            );
        }
        Ok(())
    }

    let mut state = test_state(start_route_loss_stream);
    let id = insert(&mut state);
    let mut clock = 0;
    assert_applied(settle_block(&mut state, &mut clock), []);
    let before = deck_snapshot(&mut state, id).eq;
    assert_eq!(before.gain(0), Some(GainDb::DEFAULT));
    assert_eq!(before.gain(1), Some(GainDb::DEFAULT));

    state
        .owner
        .with_deck(id, &mut |_, out, _| {
            let refused = out.together(When::Next, |out| {
                stage(out)?;
                Err::<(), _>(PlayError::NotReady)
            });
            assert!(matches!(refused, Err(PlayError::NotReady)));
        })
        .expect("held deck");
    assert_applied(settle_block(&mut state, &mut clock), []);
    assert_eq!(deck_snapshot(&mut state, id).eq, before);

    let mut accepted = None;
    state
        .owner
        .with_deck(id, &mut |_, out, _| {
            let (_, seq) = out.together(When::Next, stage).expect("accepted EQ group");
            accepted = seq;
        })
        .expect("held deck");
    assert_applied(
        settle_block(&mut state, &mut clock),
        [accepted.expect("one batch for both EQ changes")],
    );
    let applied = deck_snapshot(&mut state, id).eq;
    assert_eq!(applied.gain(0), Some(GainDb::from(-6.0)));
    assert_eq!(applied.gain(1), Some(GainDb::from(3.0)));
    assert_eq!(mix_level(&mut state, id), 1.0);
}

#[kithara::test]
fn a_foreign_deck_level_sends_no_batch_or_mutation() {
    let mut state = test_state(start_route_loss_stream);
    let known = insert(&mut state);
    let foreign = BeatGridId::allocate().expect("foreign fixture grid id");
    state.owner.begin_pass();
    let error = apply_level(&mut state, foreign, 0.5).expect_err("foreign deck");
    assert!(matches!(
        error,
        PlayError::Session(SessionError::DeckNotFound(id)) if id == foreign
    ));
    assert_eq!(mix_level(&mut state, known), 1.0);
    assert_applied(settle_block(&mut state, &mut 0), []);
    assert_eq!(mix_level(&mut state, known), 1.0);
}

#[kithara::test]
fn a_foreign_deck_refusal_does_not_cancel_a_known_decks_pending_level() {
    let mut state = test_state(start_route_loss_stream);
    let known = insert(&mut state);
    let foreign = BeatGridId::allocate().expect("foreign fixture grid id");
    state.owner.begin_pass();
    let valid = apply_level(&mut state, known, 0.2).expect("independent known level");
    let error = apply_level(&mut state, foreign, 0.3).expect_err("foreign deck");
    assert!(matches!(
        error,
        PlayError::Session(SessionError::DeckNotFound(id)) if id == foreign
    ));
    assert_eq!(mix_level(&mut state, known), 1.0);
    assert_applied(settle_block(&mut state, &mut 0), [valid]);
    assert_eq!(mix_level(&mut state, known), 0.2);
}
