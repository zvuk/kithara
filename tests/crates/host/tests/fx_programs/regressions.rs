use kithara::{
    host::DeckControl,
    platform::tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    play::{DeckEqChange, HostedDeck, Outbox, PlayWorker, ResourcePrep, TrackReceipt},
    queue::{Queue, QueueConfig},
};
use kithara_command::Outcome;
use kithara_integration_tests::{bufpool_ext::TestPools, memory_asset_store};
use kithara_render::{
    bridge::{DeckPart, Returned, SlotState},
    rt::DeckMixerConfig,
};

use super::{fixture::*, *};

struct EqInbox {
    queue: Queue<TestPools>,
    changes: UnboundedReceiver<DeckEqChange>,
    returned: UnboundedSender<DeckParameter>,
}

impl EqInbox {
    fn apply_eq(&mut self, out: &mut Outbox<'_>) {
        while let Ok(change) = self.changes.try_recv() {
            out.eq_at(When::Next, vec![change])
                .expect("public EQ admission");
        }
    }
}

impl DeckControl for EqInbox {
    type Control = kithara::queue::QueueControl<TestPools>;

    fn control(&self) -> Self::Control {
        self.queue.control()
    }
}

impl HostedDeck<TestPools> for EqInbox {
    fn drain(&mut self, pass: kithara::play::DeckPass<'_>, out: &mut Outbox<'_>) {
        self.apply_eq(out);
        self.queue.drain(pass, out);
    }

    fn tick(&mut self, pass: kithara::play::DeckPass<'_>, out: &mut Outbox<'_>) {
        self.apply_eq(out);
        self.queue.tick(pass, out);
    }

    fn settle(
        &mut self,
        receipt: TrackReceipt<'_>,
        pass: kithara::play::DeckPass<'_>,
        out: &mut Outbox<'_>,
    ) {
        if let TrackReceipt::Deck { outcome, batch, .. } = &receipt {
            if batch
                .commands
                .iter()
                .any(|part| matches!(part, DeckPart::Eq(DeckEqChange::Gain { .. })))
            {
                assert!(
                    matches!(outcome, Outcome::Applied { .. }),
                    "EQ gain admission remains applied"
                );
            }
            for part in &batch.commands {
                if let DeckPart::Returned(Returned::Program(program)) = part {
                    let _ = self.returned.send(program.parameter());
                }
            }
        }
        self.queue.settle(receipt, pass, out);
    }

    delegate::delegate! {
        to self.queue {
            fn worker(&self) -> Option<&PlayWorker<TestPools>>;
            fn mixer_config(&self) -> DeckMixerConfig;
            fn close(&mut self, out: &mut Outbox<'_>) -> Result<(), PlayError>;
            fn hold(&mut self, waker: std::task::Waker);
            fn release(&mut self);
        }
    }
}

#[kithara::test(tokio)]
#[case::mix(FxTarget::Mix)]
#[case::parameter(FxTarget::Param(0))]
async fn invalid_eq_gain_preserves_fx_programs_and_other_deck(#[case] target: FxTarget) {
    let harness = prepared_with(64, None, tone, FxMix::MIN).await;
    let mut decks = Vec::new();
    for _ in 0..2 {
        let (changes, inbox) = unbounded_channel();
        let (returned, receipts) = unbounded_channel();
        let queue = Queue::new(
            QueueConfig::builder()
                .store(memory_asset_store())
                .prep(
                    ResourcePrep::builder()
                        .worker(harness.worker().clone())
                        .build(),
                )
                .build(),
        );
        let (member, snapshot) = harness
            .host()
            .insert_observed(EqInbox {
                queue,
                changes: inbox,
                returned,
            })
            .await
            .expect("resident queue");
        let control = member.control().clone();
        harness
            .run(&control, |queue| {
                queue.fx(
                    When::Next,
                    vec![
                        DeckFxChange::Install {
                            unit: 0,
                            effect: tone(),
                        },
                        DeckFxChange::Power { unit: 0, on: true },
                    ],
                )
            })
            .await
            .expect("install declared effect");
        decks.push((member, snapshot, changes, receipts));
    }
    let _ = render(&harness, 64, 64).await;
    let start = after(harness.deck_snapshot().at, 64);
    let mut addresses = Vec::new();
    for (member, snapshot, _, _) in &decks {
        let snapshot = snapshot.lock().clone();
        let layout = snapshot.fx[0].layout.expect("FX scope");
        let parameters = vec![
            (fx(0, layout, FxTarget::Mix), [0.25, 0.75]),
            (fx(0, layout, FxTarget::Param(0)), [0.5, 1.0]),
            (
                DeckParameter::EqBand {
                    band: 0,
                    layout: snapshot.eq.layout().expect("EQ scope"),
                },
                [0.0, -6.0],
            ),
        ];
        for &(parameter, values) in &parameters {
            let program = long(parameter, start, values);
            harness
                .run(member.control(), move |queue| {
                    queue.deck_programs(vec![program])
                })
                .await
                .expect("one owned program batch");
        }
        addresses.push(
            parameters
                .into_iter()
                .map(|(parameter, _)| parameter)
                .collect::<Vec<_>>(),
        );
    }
    let _ = render(&harness, 256, 64).await;
    let owners: Vec<_> = decks
        .iter()
        .zip(&addresses)
        .map(|((_, snapshot, _, _), parameters)| {
            parameters
                .iter()
                .map(|&parameter| {
                    let field = snapshot.lock().parameter(parameter).expect("parameter");
                    assert!(field.program().is_some());
                    (field.program(), field.authority())
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let config = DeckMixerConfig::default();
    let band = config.eq_bands()
        + if target == FxTarget::Mix {
            0
        } else {
            config.fx_units()
        };
    decks[0]
        .2
        .send(DeckEqChange::Gain {
            band,
            gain: kithara::effects::GainDb::from(-3.0),
        })
        .expect("raw EQ command");
    let _ = render(&harness, 128, 64).await;
    for (deck, ((_, snapshot, _, returned), parameters)) in
        decks.iter_mut().zip(&addresses).enumerate()
    {
        assert!(
            returned.try_recv().is_err(),
            "invalid EQ gain returned a program from deck {deck}"
        );
        for (index, &parameter) in parameters.iter().enumerate() {
            let field = snapshot.lock().parameter(parameter).expect("same address");
            assert_eq!(
                (field.program(), field.authority()),
                owners[deck][index],
                "deck {deck}, {parameter:?}"
            );
        }
    }
    decks[0]
        .2
        .send(DeckEqChange::Gain {
            band: 0,
            gain: kithara::effects::GainDb::from(-3.0),
        })
        .expect("valid EQ command");
    let _ = render(&harness, 128, 64).await;
    assert_eq!(
        decks[0].3.try_recv().expect("valid EQ returns its program"),
        addresses[0][2]
    );
    assert_eq!(
        decks[0]
            .1
            .lock()
            .parameter(addresses[0][2])
            .expect("EQ field")
            .program(),
        None
    );
    assert_eq!(
        decks[1]
            .1
            .lock()
            .parameter(addresses[1][2])
            .expect("other EQ field")
            .program(),
        owners[1][2].0
    );
    harness
        .run(decks[0].0.control(), |queue| {
            queue.fx(
                When::Next,
                vec![DeckFxChange::Install {
                    unit: 0,
                    effect: tone(),
                }],
            )
        })
        .await
        .expect("replace the running effect");
    let _ = render(&harness, 128, 64).await;
    let mut returned = Vec::new();
    while let Ok(parameter) = decks[0].3.try_recv() {
        returned.push(parameter);
    }
    assert_eq!(returned.len(), 2);
    assert!(returned.contains(&addresses[0][0]) && returned.contains(&addresses[0][1]));
    for (index, &parameter) in addresses[1].iter().enumerate() {
        assert_eq!(
            decks[1]
                .1
                .lock()
                .parameter(parameter)
                .expect("other deck")
                .program(),
            owners[1][index].0
        );
    }
    drop(decks);
    harness.close().await;
}

struct Tail {
    value: [f32; 2],
    remaining: usize,
}

impl DeckEffect for Tail {
    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let [left, right] = output;
        for frame in 0..left.len() {
            let next = [input[0][frame], input[1][frame]];
            if next != [0.0; 2] {
                self.value = next;
                self.remaining = 64;
            }
            let value = if self.remaining > 0 {
                self.value
            } else {
                [0.0; 2]
            };
            (left[frame], right[frame]) = (value[0], value[1]);
            self.remaining = self.remaining.saturating_sub(usize::from(next == [0.0; 2]));
        }
    }
    fn parameters(&self) -> &[FxParamDescriptor] {
        &[]
    }
    fn set(&mut self, _: FxParamChange) {}
    fn reset(&mut self) {
        self.value = [0.0; 2];
        self.remaining = 0;
    }
    fn tail_exhausted(&self) -> bool {
        self.remaining == 0
    }
    fn update_sample_rate(&mut self, _: NonZeroU32) {}
}

fn tail() -> Box<dyn DeckEffect> {
    Box::new(Tail {
        value: [0.0; 2],
        remaining: 0,
    })
}

async fn tail_take(callback: usize) -> Vec<f32> {
    let harness = prepared_with(callback, None, tail, FxMix::MAX).await;
    harness
        .with_queue(|queue| {
            queue.fx(
                When::Next,
                vec![
                    DeckFxChange::Mix {
                        unit: 0,
                        value: FxMix::MIN,
                    },
                    DeckFxChange::Mix {
                        unit: 1,
                        value: FxMix::MIN,
                    },
                ],
            )
        })
        .await
        .expect("turn dry");
    let _ = render(&harness, WARM, callback).await;
    harness
        .with_queue(kithara::queue::QueueControl::pause)
        .await;
    let silent = render(&harness, WARM, callback).await;
    assert!(
        silent[silent.len() - 128..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    assert!(
        harness
            .deck_snapshot()
            .slots
            .iter()
            .all(|slot| slot.state != SlotState::Playing)
    );
    let parameter = mix(0, layout(&harness, 0));
    assert_eq!(
        harness
            .deck_snapshot()
            .parameter(parameter)
            .expect("dry mix")
            .value(),
        Some(0.0)
    );
    let first = frame(harness.host().position() + u64::try_from(FIRST).expect("offset"));
    let program = DeckProgram::new(
        parameter,
        vec![
            ParameterKnot::new(first, 0.0, CurveShape::LINEAR).expect("first knot"),
            ParameterKnot::new(
                after(first, i64::try_from(SEGMENT).expect("duration")),
                1.0,
                CurveShape::HOLD,
            )
            .expect("last knot"),
        ],
    )
    .expect("wet program");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![program]))
        .await
        .expect("raise the frozen tail");
    let take = render(&harness, CAPTURE, callback).await;
    for (frame, channels) in take.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let expected = if (FIRST + 1..FIRST + 65).contains(&frame) {
            INPUT * (frame - FIRST).to_f32().expect("frame") / SEGMENT.to_f32().expect("duration")
        } else {
            0.0
        };
        assert!(
            channels
                .iter()
                .all(|sample| sample.to_bits() == expected.to_bits()),
            "tail frame {frame}, callback {callback}: {channels:?} vs {expected}"
        );
    }
    let field = harness
        .deck_snapshot()
        .parameter(parameter)
        .expect("held mix");
    assert_eq!(field.value(), Some(1.0));
    assert!(field.program().is_some());
    harness.close().await;
    take
}

#[kithara::test(tokio)]
async fn a_mix_program_wakes_a_frozen_tail_on_the_same_frames_for_two_partitions() {
    let regular = tail_take(64).await;
    let prime = tail_take(13).await;
    assert!(
        regular
            .iter()
            .map(|sample| sample.to_bits())
            .eq(prime.iter().map(|sample| sample.to_bits()))
    );
}

struct Extreme {
    domain: [FxParamDescriptor; 1],
    value: f64,
}

impl DeckEffect for Extreme {
    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        for (input, output) in input.into_iter().zip(output) {
            for (input, output) in input.iter().zip(output) {
                *output = (f64::from(*input) * (self.value / 1.0e308))
                    .to_f32()
                    .expect("bounded normalized gain");
            }
        }
    }
    fn parameters(&self) -> &[FxParamDescriptor] {
        &self.domain
    }
    fn set(&mut self, change: FxParamChange) {
        assert!(
            self.domain[change.index].contains(change.value),
            "effect received invalid native value {}",
            change.value
        );
        self.value = change.value;
    }
    fn reset(&mut self) {}
    fn tail_exhausted(&self) -> bool {
        true
    }
    fn update_sample_rate(&mut self, _: NonZeroU32) {}
}

fn extreme() -> Box<dyn DeckEffect> {
    Box::new(Extreme {
        domain: [FxParamDescriptor::checked(
            -1.0e308..=1.0e308,
            FxParamUnit::Ratio,
            Some(FxParamMapping::Linear),
        )
        .expect("finite domain")],
        value: 1.0e308,
    })
}

#[kithara::test(tokio)]
async fn finite_extreme_program_values_stay_inside_the_effect_descriptor() {
    let harness = prepared_with(64, None, extreme, FxMix::MAX).await;
    let parameter = fx(0, layout(&harness, 0), FxTarget::Param(0));
    let start = after(harness.deck_snapshot().at, 64);
    let program = DeckProgram::new(
        parameter,
        vec![
            ParameterKnot::new(start, -1.0e308, CurveShape::LINEAR).expect("finite first"),
            ParameterKnot::new(after(start, 128), 1.0e308, CurveShape::HOLD).expect("finite last"),
        ],
    )
    .expect("valid native program");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![program]))
        .await
        .expect("admit finite endpoints");
    let take = render(&harness, CAPTURE, 64).await;
    assert!(take.iter().all(|sample| sample.is_finite()));
    assert_eq!(
        harness
            .deck_snapshot()
            .parameter(parameter)
            .expect("endpoint")
            .value()
            .expect("delivered")
            .to_bits(),
        1.0e308_f64.to_bits()
    );
    harness.close().await;
}
