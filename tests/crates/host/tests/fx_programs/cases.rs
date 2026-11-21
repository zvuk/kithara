use super::{fixture::*, *};

/// What unit 0 sounds under a dry/wet program from fully dry to fully wet,
/// called back `callback` frames at a time.
async fn mix_take(callback: usize) -> Vec<f32> {
    let harness = prepared(callback).await;
    let start = harness.host().position();
    let first = frame(start + u64::try_from(FIRST).expect("offset fits u64"));
    let program = DeckProgram::new(
        mix(0, layout(&harness, 0)),
        vec![
            ParameterKnot::new(
                first,
                0.0,
                CurveShape::cubic(0.0, 1.0).expect("bounded S curve"),
            )
            .expect("first knot"),
            ParameterKnot::new(
                after(first, i64::try_from(SEGMENT).expect("segment fits i64")),
                1.0,
                CurveShape::HOLD,
            )
            .expect("last knot"),
        ],
    )
    .expect("a dry/wet program");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![program]))
        .await
        .expect("the deck takes the dry/wet program");
    let take = render(&harness, CAPTURE, callback).await;
    for (offset, channels) in take.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let wet = if offset < FIRST {
            0.0
        } else if offset >= FIRST + SEGMENT {
            1.0
        } else {
            let t = (offset - FIRST).to_f64().expect("local frame") / 128.0;
            3.0 * t * t - 2.0 * t * t * t
        }
        .to_f32()
        .expect("a dry/wet fits f32");
        let expected = INPUT * (1.0 - wet) + halve(INPUT) * wet;
        for sample in channels {
            assert!(
                (sample - expected).abs() <= f32::EPSILON,
                "frame {offset}, callback {callback}: {sample} against {expected}"
            );
        }
    }
    let snapshot = harness.deck_snapshot();
    let applied = snapshot
        .parameter(mix(0, layout(&harness, 0)))
        .expect("unit 0 dry/wet");
    assert_eq!(applied.value(), Some(1.0), "the last knot holds");
    assert!(applied.program().is_some());
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    take
}

#[kithara::test(tokio)]
async fn a_dry_wet_program_applies_on_its_frames_under_two_partitions() {
    let regular = mix_take(64).await;
    let prime = mix_take(13).await;
    assert!(
        regular
            .iter()
            .map(|sample| sample.to_bits())
            .eq(prime.iter().map(|sample| sample.to_bits())),
        "the callback partition changes no output bit"
    );
}

#[kithara::test(tokio)]
#[case::dry_wet(FxTarget::Mix)]
#[case::parameter(FxTarget::Param(0))]
async fn a_replaced_effect_returns_only_its_unit_program(#[case] target: FxTarget) {
    let harness = match target {
        FxTarget::Mix => prepared(64).await,
        FxTarget::Param(_) => prepared_with(64, None, tone, FxMix::MAX).await,
    };
    let start = after(harness.deck_snapshot().at, 64);
    let programs = vec![
        long(DeckParameter::Crossfader, start, [1.0, 0.5]),
        long(fx(0, layout(&harness, 0), target), start, [0.0, 1.0]),
        long(fx(1, layout(&harness, 1), target), start, [0.0, 1.0]),
    ];
    harness
        .with_queue(move |queue| queue.deck_programs(programs))
        .await
        .expect("the deck takes three programs");
    let _ = render(&harness, 1024, 64).await;
    let replaced = layout(&harness, 0);
    let swap = vec![DeckFxChange::Install {
        unit: 0,
        effect: Box::new(Curve(halve)),
    }];
    harness
        .with_queue(move |queue| queue.fx(When::Next, swap))
        .await
        .expect("the deck takes the new effect");
    let _ = render(&harness, 256, 64).await;
    let snapshot = harness.deck_snapshot();
    let unit = |unit: usize| {
        snapshot
            .parameter(fx(
                unit,
                snapshot.fx[unit].layout.expect("current unit"),
                target,
            ))
            .expect("current parameter metadata")
    };
    assert_ne!(snapshot.fx[0].layout, Some(replaced), "unit 0 advanced");
    assert_eq!(
        unit(0).program(),
        None,
        "the replaced unit's program stopped"
    );
    assert!(unit(1).program().is_some(), "the other unit's program runs");
    let crossfader = snapshot
        .parameter(DeckParameter::Crossfader)
        .expect("crossfader");
    assert!(
        crossfader.program().is_some(),
        "the crossfader program runs"
    );
    let stale = long(fx(0, replaced, target), after(snapshot.at, 64), [0.0, 1.0]);
    assert!(
        matches!(
            harness
                .with_queue(move |queue| queue.deck_programs(vec![stale]))
                .await,
            Err(QueueError::Play(PlayError::NotReady))
        ),
        "a program naming the replaced effect is refused"
    );
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn a_dry_wet_program_on_an_empty_unit_refuses_its_whole_batch() {
    let harness = prepared(64).await;
    harness
        .with_queue(|queue| queue.fx(When::Next, vec![DeckFxChange::Clear { unit: 1 }]))
        .await
        .expect("the deck empties unit 1");
    let _ = render(&harness, 64, 64).await;
    let start = after(harness.deck_snapshot().at, 64);
    let programs = vec![
        long(DeckParameter::Trim, start, [1.0, 0.5]),
        long(mix(1, layout(&harness, 1)), start, [0.0, 1.0]),
    ];
    harness
        .with_queue(move |queue| queue.deck_programs(programs))
        .await
        .expect("the batch reaches the deck");
    let take = render(&harness, 1024, 64).await;
    assert!(
        take.iter()
            .all(|sample| sample.to_bits() == INPUT.to_bits()),
        "no part of the refused batch sounds"
    );
    let snapshot = harness.deck_snapshot();
    let trim = snapshot.parameter(DeckParameter::Trim).expect("trim");
    assert_eq!(trim.program(), None);
    assert_eq!(trim.value(), Some(1.0));
    assert_eq!(snapshot.fx[1].mix.program(), None);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

/// What unit 0 sounds under programs moving its linear parameter from 1 to
/// 0.5 and its logarithmic one from 1 to 2 over the same frames, called back
/// `callback` frames at a time. The product stays under the session limiter.
async fn parameter_take(callback: usize) -> Vec<f32> {
    let harness = prepared_with(callback, None, tone, FxMix::MAX).await;
    let layout = layout(&harness, 0);
    let start = harness.host().position();
    let first = frame(start + u64::try_from(FIRST).expect("offset fits u64"));
    let last = after(first, i64::try_from(SEGMENT).expect("segment fits i64"));
    let moving = |index: usize, to: f64| {
        DeckProgram::new(
            fx(0, layout, FxTarget::Param(index)),
            vec![
                ParameterKnot::new(first, 1.0, CurveShape::LINEAR).expect("first knot"),
                ParameterKnot::new(last, to, CurveShape::HOLD).expect("last knot"),
            ],
        )
        .expect("an effect parameter program")
    };
    let programs = vec![moving(0, 0.5), moving(1, 2.0)];
    harness
        .with_queue(move |queue| queue.deck_programs(programs))
        .await
        .expect("the deck takes the effect parameter programs");
    let take = render(&harness, CAPTURE, callback).await;
    for (offset, channels) in take.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let progress = offset
            .saturating_sub(FIRST)
            .min(SEGMENT)
            .to_f64()
            .expect("local frame")
            / SEGMENT.to_f64().expect("segment");
        let expected = f64::from(INPUT) * (1.0 - 0.5 * progress) * 2.0_f64.powf(progress);
        for sample in channels {
            let sample = f64::from(*sample);
            assert!(
                (sample - expected).abs() <= expected * 1e-6,
                "frame {offset}, callback {callback}: {sample} against {expected}"
            );
        }
    }
    let snapshot = harness.deck_snapshot();
    for (index, value) in [(0, 0.5), (1, 2.0)] {
        let applied = snapshot
            .parameter(fx(0, layout, FxTarget::Param(index)))
            .expect("the unit's effect parameter");
        assert!(
            (applied.value().expect("delivered parameter value") - value).abs() <= 1e-9,
            "the last knot holds"
        );
        assert!(applied.program().is_some());
    }
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    take
}

#[kithara::test(tokio)]
async fn effect_parameter_programs_apply_on_their_frames_under_two_partitions() {
    let regular = parameter_take(64).await;
    let prime = parameter_take(13).await;
    assert!(
        regular
            .iter()
            .map(|sample| sample.to_bits())
            .eq(prime.iter().map(|sample| sample.to_bits())),
        "the callback partition changes no output bit"
    );
}

#[kithara::test(tokio)]
#[case::first_install(false)]
#[case::replacement(true)]
async fn an_effect_parameter_program_delivers_zero_as_its_first_value(#[case] replacement: bool) {
    let harness = prepared_with(64, None, tone, FxMix::MAX).await;
    if replacement {
        harness
            .with_queue(|queue| {
                queue.fx(
                    When::Next,
                    vec![DeckFxChange::Param {
                        unit: 0,
                        change: FxParamChange {
                            index: 0,
                            value: 0.0,
                        },
                    }],
                )
            })
            .await
            .expect("the old effect receives zero");
        assert!(
            render(&harness, 64, 64)
                .await
                .iter()
                .all(|sample| *sample == 0.0)
        );
        harness
            .with_queue(|queue| {
                queue.fx(
                    When::Next,
                    vec![DeckFxChange::Install {
                        unit: 0,
                        effect: tone(),
                    }],
                )
            })
            .await
            .expect("the replacement has its own prepared state");
        assert!(
            render(&harness, 64, 64)
                .await
                .iter()
                .all(|sample| *sample == INPUT)
        );
    }
    let parameter = fx(0, layout(&harness, 0), FxTarget::Param(0));
    let unknown = harness
        .deck_snapshot()
        .parameter(parameter)
        .expect("parameter metadata");
    assert_eq!(unknown.value(), None);
    assert_eq!(unknown.program(), None);
    assert!(unknown.authority().is_some());
    assert_eq!(
        harness
            .deck_snapshot()
            .parameter(fx(0, layout(&harness, 0), FxTarget::Param(1)))
            .expect("positive native domain")
            .value(),
        None
    );
    let first = frame(harness.host().position() + u64::try_from(FIRST).expect("offset"));
    let program = DeckProgram::new(
        parameter,
        vec![ParameterKnot::new(first, 0.0, CurveShape::HOLD).expect("zero knot")],
    )
    .expect("a zero-valued parameter program");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![program]))
        .await
        .expect("the deck takes the zero-valued program");
    let take = render(&harness, CAPTURE, 64).await;
    for (frame, channels) in take.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let expected = if frame < FIRST { INPUT } else { 0.0 };
        assert!(
            channels.iter().all(|sample| *sample == expected),
            "frame {frame}: {channels:?} against {expected}"
        );
    }
    let delivered = harness
        .deck_snapshot()
        .parameter(parameter)
        .expect("delivered metadata");
    assert_eq!(delivered.value(), Some(0.0));
    assert!(delivered.program().is_some());
    harness
        .with_queue(|queue| queue.fx(When::Next, vec![DeckFxChange::Clear { unit: 0 }]))
        .await
        .expect("clear the effect");
    let _ = render(&harness, 64, 64).await;
    let snapshot = harness.deck_snapshot();
    assert_eq!(
        snapshot.parameter(parameter),
        None,
        "the old layout is stale"
    );
    assert_eq!(snapshot.fx[0].params[0].value(), None);
    assert_eq!(snapshot.fx[0].params[0].program(), None);
    assert_ne!(snapshot.fx[0].params[0].authority(), delivered.authority());
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio)]
#[case::manual(false)]
#[case::held_program(true)]
async fn effect_parameter_controls_survive_power_cycles(#[case] programmed: bool) {
    let harness = prepared_with(64, None, tone, FxMix::MAX).await;
    let parameter = fx(0, layout(&harness, 0), FxTarget::Param(0));
    if programmed {
        let knot = ParameterKnot::new(harness.deck_snapshot().at, 0.25, CurveShape::HOLD)
            .expect("held native value");
        let program = DeckProgram::new(parameter, vec![knot]).expect("parameter program");
        harness
            .with_queue(move |queue| queue.deck_programs(vec![program]))
            .await
            .expect("the deck takes the program");
    } else {
        harness
            .with_queue(|queue| {
                queue.fx(
                    When::Next,
                    vec![DeckFxChange::Param {
                        unit: 0,
                        change: FxParamChange {
                            index: 0,
                            value: 0.25,
                        },
                    }],
                )
            })
            .await
            .expect("the deck takes the manual value");
    }
    let expected = (INPUT * 0.25).to_bits();
    assert!(
        render(&harness, 256, 64)
            .await
            .iter()
            .all(|sample| sample.to_bits() == expected)
    );
    let before = harness
        .deck_snapshot()
        .parameter(parameter)
        .expect("delivered value");
    assert_eq!(before.value(), Some(0.25));
    assert_eq!(before.program().is_some(), programmed);
    harness
        .with_queue(|queue| {
            queue.fx(
                When::Next,
                vec![
                    DeckFxChange::Power { unit: 0, on: false },
                    DeckFxChange::Power { unit: 0, on: true },
                ],
            )
        })
        .await
        .expect("power cycles the unit without changing its controls");
    let take = render(&harness, 256, 64).await;
    assert!(
        take.iter().all(|sample| sample.to_bits() == expected),
        "power preserves the native control in the audible PCM"
    );
    assert_eq!(harness.deck_snapshot().parameter(parameter), Some(before));
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio)]
#[case::undeclared_index(false, FxTarget::Param(3), [1.0, 2.0])]
#[case::out_of_range_knot(false, FxTarget::Param(0), [1.0, 3.0])]
#[case::stepped_parameter(false, FxTarget::Param(2), [0.0, 3.0])]
#[case::empty_unit(true, FxTarget::Param(0), [1.0, 2.0])]
async fn an_effect_parameter_program_the_unit_cannot_take_refuses_its_whole_batch(
    #[case] empty: bool,
    #[case] target: FxTarget,
    #[case] knots: [f64; 2],
) {
    let harness = prepared_with(64, None, tone, FxMix::MAX).await;
    if empty {
        harness
            .with_queue(|queue| queue.fx(When::Next, vec![DeckFxChange::Clear { unit: 1 }]))
            .await
            .expect("the deck empties unit 1");
        let _ = render(&harness, 64, 64).await;
    }
    let unit = usize::from(empty);
    let start = after(harness.deck_snapshot().at, 64);
    let refused = fx(unit, layout(&harness, unit), target);
    let programs = vec![
        long(DeckParameter::Trim, start, [1.0, 0.5]),
        long(refused, start, knots),
    ];
    harness
        .with_queue(move |queue| queue.deck_programs(programs))
        .await
        .expect("the batch reaches the deck");
    let take = render(&harness, 1024, 64).await;
    assert!(
        take.iter()
            .all(|sample| (sample - INPUT).abs() <= f32::EPSILON),
        "no part of the refused batch sounds"
    );
    let snapshot = harness.deck_snapshot();
    let trim = snapshot.parameter(DeckParameter::Trim).expect("trim");
    assert_eq!(trim.program(), None);
    assert_eq!(trim.value(), Some(1.0));
    assert_eq!(snapshot.parameter(refused).and_then(|p| p.program()), None);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn a_program_on_every_deck_and_effect_parameter_is_admitted_at_once() {
    let harness = prepared_with(64, None, tone, FxMix::MAX).await;
    let start = after(harness.deck_snapshot().at, 64);
    let eq = harness
        .deck_snapshot()
        .eq
        .layout()
        .expect("observed EQ layout");
    let (first, second) = (layout(&harness, 0), layout(&harness, 1));
    let parameters = [
        (DeckParameter::Trim, [1.0, 0.5]),
        (DeckParameter::Volume, [1.0, 0.5]),
        (DeckParameter::Crossfader, [1.0, 0.5]),
        (DeckParameter::Filter, [0.0, 0.25]),
        (
            DeckParameter::EqBand {
                band: 0,
                layout: eq,
            },
            [0.0, -3.0],
        ),
        (mix(0, first), [1.0, 0.5]),
        (mix(1, second), [0.0, 0.5]),
        (fx(0, first, FxTarget::Param(0)), [1.0, 2.0]),
        (fx(0, first, FxTarget::Param(1)), [1.0, 16.0]),
        (fx(1, second, FxTarget::Param(0)), [1.0, 0.5]),
        (fx(1, second, FxTarget::Param(1)), [16.0, 1.0]),
    ];
    let programs = parameters
        .iter()
        .map(|(parameter, knots)| long(*parameter, start, *knots))
        .collect::<Vec<_>>();
    harness
        .with_queue(move |queue| queue.deck_programs(programs))
        .await
        .expect("the deck takes a program on every parameter");
    let _ = render(&harness, 256, 64).await;
    let snapshot = harness.deck_snapshot();
    for (parameter, _) in parameters {
        let applied = snapshot
            .parameter(parameter)
            .expect("an observed parameter");
        assert!(
            applied.program().is_some(),
            "{parameter:?} runs its program"
        );
    }
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn an_effect_over_the_capacity_is_refused_with_its_whole_batch() {
    let harness = prepared_with(64, Some(2), halving, FxMix::MIN).await;
    let wet = |unit| DeckFxChange::Mix {
        unit,
        value: FxMix::MAX,
    };
    harness
        .with_queue(move |queue| {
            queue.fx(
                When::Next,
                vec![
                    wet(0),
                    DeckFxChange::Install {
                        unit: 1,
                        effect: tone(),
                    },
                ],
            )
        })
        .await
        .expect("the batch reaches the deck");
    let take = render(&harness, 4096, 64).await;
    assert!(
        take.iter()
            .all(|sample| (sample - INPUT).abs() <= f32::EPSILON),
        "no part of the refused batch sounds"
    );
    harness
        .with_queue(move |queue| queue.fx(When::Next, vec![wet(0)]))
        .await
        .expect("the deck takes the batch without the oversized effect");
    let take = render(&harness, 4096, 64).await;
    assert!(
        take[take.len() - 128..]
            .iter()
            .all(|sample| (sample - halve(INPUT)).abs() <= f32::EPSILON),
        "the unit turns wet without it"
    );
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
