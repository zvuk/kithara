use kithara_command::Target;
use kithara_dsp::filter::{Biquad, Coefficients, Hertz, Type};
use kithara_effects::filter::{
    Cutoff, FilterConfig, FilterEffect, FilterParameter, FilterSettingsChange, Kind, Q,
};

use super::{
    filter::{binding, bits, filter, part, prepared},
    *,
};
use crate::bridge::{CurveShape, DeckFilterChange, FilterProgram, ParameterKnot};

fn setup() -> (TestMixer, TestEnds, usize) {
    let (mut mixer, mut ends) = prepared();
    let observed = binding(&mut ends);
    send(
        &mut ends,
        at(BLOCK),
        vec![part(
            observed,
            DeckFilterChange::Install(Box::new(filter(1_000.0))),
        )],
    );
    render_frames(&mut mixer, BLOCK, 1);
    assert_eq!(receipts(&mut ends).len(), 1);
    (mixer, ends, BLOCK + 1)
}

fn program(
    field: FilterParameter,
    start: usize,
    end: usize,
    left: f64,
    right: f64,
) -> FilterProgram {
    FilterProgram::new(
        field,
        vec![
            ParameterKnot::new(frame_at(start), left, CurveShape::LINEAR).expect("left"),
            ParameterKnot::new(frame_at(end), right, CurveShape::HOLD).expect("right"),
        ],
    )
    .expect("program")
}

fn program_part(ends: &mut TestEnds, program: FilterProgram) -> DeckPart {
    let snapshot = ends.deck.snapshot.read();
    DeckPart::FilterProgram {
        binding: snapshot.filter.binding().expect("observed Filter"),
        authority: snapshot.filter.authority(program.parameter()),
        program,
    }
}

fn oracle() -> FilterEffect {
    let mut filter = filter(1_000.0);
    filter
        .process(&mut [&mut [1.0][..], &mut [1.0][..]], 0..1)
        .expect("installed first frame");
    filter
}

fn tick(oracle: &mut FilterEffect) -> f32 {
    let (mut left, mut right) = ([1.0], [1.0]);
    oracle
        .process(&mut [&mut left[..], &mut right[..]], 0..1)
        .expect("reference frame");
    assert_eq!(left.map(f32::to_bits), right.map(f32::to_bits));
    left[0]
}

#[kithara::test]
fn cutoff_curve_is_geometric_frame_exact_and_has_no_second_smoothing() {
    type Progression = fn(f64) -> f64;
    let shapes: [(CurveShape, Progression); 3] = [
        (CurveShape::HOLD, |_| 0.0),
        (CurveShape::LINEAR, |t| t),
        (CurveShape::cubic(0.0, 1.0).expect("S shape"), |t| {
            3.0 * t * t - 2.0 * t * t * t
        }),
    ];
    for (shape, progression) in shapes {
        for block in [1_usize, 3, 7, 32] {
            let (mut mixer, mut ends, origin) = setup();
            let sweep = FilterProgram::new(
                FilterParameter::CutoffHz,
                vec![
                    ParameterKnot::new(frame_at(origin + 5), 200.0, shape).expect("left"),
                    ParameterKnot::new(frame_at(origin + 21), 800.0, CurveShape::HOLD)
                        .expect("right"),
                ],
            )
            .expect("sweep");
            let command = program_part(&mut ends, sweep);
            let seq = send(&mut ends, at(origin + 5), vec![command]);
            let observed = binding(&mut ends);
            let refused_gain = send(
                &mut ends,
                at(origin + 10),
                vec![part(
                    observed,
                    DeckFilterChange::Configure(FilterSettingsChange::GainDb(GainDb::MAX)),
                )],
            );
            let mono = FilterEffect::new(
                AudioSpec::new(1, super::shape().sample_rate),
                FilterConfig::builder(pools())
                    .kind(Kind::LowPass)
                    .cutoff_hz(Cutoff::checked(1_000.0).expect("cutoff"))
                    .build()
                    .expect("settings"),
            )
            .expect("prepared mono instance");
            let refused_shape = send(
                &mut ends,
                at(origin + 11),
                vec![part(observed, DeckFilterChange::Install(Box::new(mono)))],
            );
            let mut actual = Vec::new();
            for offset in (0..32).step_by(block) {
                actual.extend(render_frames(
                    &mut mixer,
                    origin + offset,
                    block.min(32 - offset),
                ));
            }
            let mut oracle = oracle();
            let expected: Vec<_> = (0_u32..32)
                .map(|frame| {
                    if frame >= 5 {
                        let cutoff = if frame >= 21 {
                            800.0
                        } else {
                            200.0 * 4.0_f64.powf(progression(f64::from(frame - 5) / 16.0))
                        };
                        oracle
                            .set_explicit(FilterSettingsChange::CutoffHz(
                                Cutoff::checked(cutoff).expect("reference cutoff"),
                            ))
                            .expect("explicit reference");
                    }
                    tick(&mut oracle)
                })
                .collect();
            assert_eq!(bits(&actual), bits(&expected));
            assert_ne!(bits(&actual), bits(&[1.0; 32]));
            let replies = receipts(&mut ends);
            assert_eq!(replies.len(), 3);
            assert_applied_at(outcome_of(&replies, seq), origin + 5);
            assert!(matches!(
                outcome_of(&replies, refused_gain),
                Outcome::Rejected(Rejection::Refused(DeckRefusal::Filter(
                    kithara_effects::filter::FilterError::Parameters
                )))
            ));
            assert!(matches!(
                outcome_of(&replies, refused_shape),
                Outcome::Rejected(Rejection::Refused(DeckRefusal::FilterShape))
            ));
            let returned = &replies
                .iter()
                .find(|(returned, _, _)| *returned == seq)
                .expect("original sweep receipt")
                .2;
            assert!(
                matches!(&returned[..], [DeckPart::FilterProgram { program, .. }] if program.start() == frame_at(origin + 5) && program.end() == frame_at(origin + 21))
            );
            let snapshot = ends.deck.snapshot.read();
            assert_eq!(snapshot.filter.binding(), Some(observed));
            assert_eq!(
                snapshot.filter.program(FilterParameter::CutoffHz),
                Some(seq)
            );
            assert_eq!(
                f64::from(snapshot.filter.settings().expect("settings").cutoff_hz()),
                800.0
            );
            assert_eq!(snapshot.metrics.underruns(), 0);
        }
    }
}

#[kithara::test]
fn same_frame_manual_takeover_preserves_the_other_filter_trajectory() {
    let mut outputs = Vec::new();
    for manual_first in [false, true] {
        let (mut mixer, mut ends, origin) = setup();
        let cutoff = program_part(
            &mut ends,
            program(FilterParameter::CutoffHz, origin, origin + 16, 200.0, 800.0),
        );
        let q = program_part(
            &mut ends,
            program(FilterParameter::Q, origin, origin + 31, 0.75, 2.0),
        );
        send(&mut ends, at(origin), vec![cutoff]);
        let q_seq = send(&mut ends, at(origin), vec![q]);
        let mut actual = render_frames(&mut mixer, origin, 8);
        let replacement = FilterProgram::new(
            FilterParameter::CutoffHz,
            vec![
                ParameterKnot::new(frame_at(origin + 10), 20_000.0, CurveShape::HOLD)
                    .expect("replacement point"),
            ],
        )
        .expect("replacement");
        let replacement = program_part(&mut ends, replacement);
        let manual = part(
            binding(&mut ends),
            DeckFilterChange::Configure(FilterSettingsChange::CutoffHz(
                Cutoff::checked(1_200.0).expect("manual target"),
            )),
        );
        let (manual_seq, replacement_seq) = if manual_first {
            let manual = send(&mut ends, at(origin + 10), vec![manual]);
            (manual, send(&mut ends, at(origin + 10), vec![replacement]))
        } else {
            let replacement = send(&mut ends, at(origin + 10), vec![replacement]);
            (send(&mut ends, at(origin + 10), vec![manual]), replacement)
        };
        actual.extend(render_frames(&mut mixer, origin + 8, 24));
        let mut oracle = oracle();
        let expected: Vec<_> = (0_u32..32)
            .map(|frame| {
                if frame <= 10 {
                    let cutoff = 200.0 * 4.0_f64.powf(f64::from(frame) / 16.0);
                    oracle
                        .set_explicit(FilterSettingsChange::CutoffHz(
                            Cutoff::checked(cutoff).expect("cutoff"),
                        ))
                        .expect("old curve point");
                }
                if frame == 10 {
                    oracle
                        .configure(FilterSettingsChange::CutoffHz(
                            Cutoff::checked(1_200.0).expect("manual target"),
                        ))
                        .expect("manual law");
                }
                let q = 0.75 + 1.25 * (f64::from(frame) / 31.0);
                oracle
                    .set_explicit(FilterSettingsChange::Q(Q::checked(q).expect("Q")))
                    .expect("independent Q point");
                tick(&mut oracle)
            })
            .collect();
        assert_eq!(bits(&actual), bits(&expected));
        let replies = receipts(&mut ends);
        assert_applied_at(outcome_of(&replies, manual_seq), origin + 10);
        if manual_first {
            assert!(matches!(
                outcome_of(&replies, replacement_seq),
                Outcome::Rejected(Rejection::Refused(DeckRefusal::FilterOutdated))
            ));
        } else {
            assert_applied_at(outcome_of(&replies, replacement_seq), origin + 10);
        }
        let snapshot = ends.deck.snapshot.read();
        assert_eq!(
            snapshot.filter.authority(FilterParameter::CutoffHz),
            Some(manual_seq)
        );
        assert_eq!(snapshot.filter.authority(FilterParameter::Q), None);
        assert_eq!(snapshot.filter.program(FilterParameter::Q), Some(q_seq));
        assert_eq!(snapshot.filter.program(FilterParameter::CutoffHz), None);
        assert_eq!(
            f64::from(snapshot.filter.settings().expect("settings").cutoff_hz()),
            1_200.0
        );
        assert_eq!(
            f64::from(snapshot.filter.settings().expect("settings").q()),
            2.0
        );
        assert_eq!(snapshot.metrics.underruns(), 0);
        outputs.push(actual);
    }
    assert_eq!(
        outputs.first().map(|pcm| bits(pcm)),
        outputs.last().map(|pcm| bits(pcm))
    );
}

#[kithara::test]
fn cubic_cutoff_q_and_shelf_gain_reach_each_frame_of_the_resident_kernel() {
    let (mut mixer, mut ends) = prepared();
    let config = FilterConfig::builder(pools())
        .kind(Kind::LowShelf)
        .cutoff_hz(Cutoff::checked(1_000.0).expect("cutoff"))
        .build()
        .expect("settings");
    let shelf = FilterEffect::new(AudioSpec::new(2, shape().sample_rate), config).expect("shelf");
    let observed = binding(&mut ends);
    send(
        &mut ends,
        at(BLOCK),
        vec![part(observed, DeckFilterChange::Install(Box::new(shelf)))],
    );
    render_frames(&mut mixer, BLOCK, 1);
    assert_eq!(receipts(&mut ends).len(), 1);
    let origin = BLOCK + 1;
    let shape = CurveShape::cubic(0.0, 1.0).expect("S shape");
    let mut submitted = Vec::new();
    for (field, left, right) in [
        (FilterParameter::CutoffHz, 200.0, 800.0),
        (FilterParameter::Q, 0.1, 10.0),
        (FilterParameter::GainDb, -24.0, 6.0),
    ] {
        let program = FilterProgram::new(
            field,
            vec![
                ParameterKnot::new(frame_at(origin), left, shape).expect("left"),
                ParameterKnot::new(frame_at(origin + 32), right, CurveShape::HOLD).expect("right"),
            ],
        )
        .expect("trajectory");
        let command = program_part(&mut ends, program);
        submitted.push((field, send(&mut ends, at(origin), vec![command])));
    }
    let actual = render_frames(&mut mixer, origin, 33);
    let pools = pools();
    let mut planes = [
        pools.get_with_len::<f32>(256).expect("left"),
        pools.get_with_len::<f32>(256).expect("right"),
    ];
    let mut kernel =
        Biquad::new(NonZeroUsize::new(2).expect("stereo"), NonZeroUsize::MIN).expect("kernel");
    let coefficients = |cutoff, q, gain| {
        Coefficients::from_params(
            Type::LowShelf(gain),
            Hertz::from_hz(f64::from(RATE)).expect("rate"),
            Hertz::from_hz(cutoff).expect("cutoff"),
            q,
        )
        .expect("causal coefficients")
    };
    kernel
        .retune(0, coefficients(1_000.0, f64::from(Q::default()), 0.0))
        .expect("initial shelf");
    for plane in &mut planes {
        plane[0] = 1.0;
    }
    kernel.process(&mut planes, 0..1).expect("installed frame");
    let expected: Vec<_> = (0_u32..=32)
        .map(|frame| {
            let t = f64::from(frame) / 32.0;
            let progress = 3.0 * t * t - 2.0 * t * t * t;
            let cutoff = if frame == 32 {
                800.0
            } else {
                200.0 * 4.0_f64.powf(progress)
            };
            let q = if frame == 32 {
                10.0
            } else {
                0.1 + (10.0 - 0.1) * progress
            };
            let db = if frame == 32 {
                6.0
            } else {
                -24.0 + 30.0 * progress
            };
            let gain = GainDb::checked(db.to_f32().expect("canonical dB")).expect("admitted gain");
            kernel
                .retune(0, coefficients(cutoff, q, f64::from(f32::from(gain))))
                .expect("stable joint reference");
            for plane in &mut planes {
                plane[0] = 1.0;
            }
            kernel.process(&mut planes, 0..1).expect("reference frame");
            assert_eq!(planes[0][0].to_bits(), planes[1][0].to_bits());
            planes[0][0]
        })
        .collect();
    assert_eq!(bits(&actual), bits(&expected));
    assert!(actual.iter().all(|sample| sample.is_finite()));
    let replies = receipts(&mut ends);
    for (field, seq) in submitted {
        assert_applied_at(outcome_of(&replies, seq), origin);
        assert_eq!(ends.deck.snapshot.read().filter.program(field), Some(seq));
    }
    let snapshot = ends.deck.snapshot.read();
    let settings = snapshot.filter.settings().expect("confirmed settings");
    assert_eq!(f64::from(settings.cutoff_hz()), 800.0);
    assert_eq!(f64::from(settings.q()), 10.0);
    assert_eq!(settings.gain_db(), GainDb::MAX);
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn switched_high_q_trajectory_is_refused_or_keeps_normalized_pcm_finite() {
    const PERIOD: usize = 552;
    const CYCLES: usize = FilterProgram::MAX_KNOTS / 2;
    const TAKE: usize = PERIOD * CYCLES;
    let (mut mixer, mut ends) = mixer_with_config(DeckMixerConfig::default(), 32);
    render_frames(&mut mixer, 0, 1);
    let config = FilterConfig::builder(pools())
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::MIN)
        .q(Q::MAX)
        .build()
        .expect("stable endpoint settings");
    let effect = FilterEffect::new(AudioSpec::new(2, shape().sample_rate), config.clone())
        .expect("prepared high-Q filter");
    let mut reference = FilterEffect::new(AudioSpec::new(2, shape().sample_rate), config)
        .expect("unchanged stable reference");
    send(
        &mut ends,
        at(1),
        vec![
            DeckPart::Attach {
                slot: A,
                item_id: kithara_events::TrackId::allocate(),
                pcm: pcm_samples(
                    "normalized switched filter",
                    1.0,
                    &vec![1.0; (TAKE + 2) * 2],
                    false,
                ),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Crossfade(
                    CrossfadeSettings::new(0.0, CrossfadeCurve::Linear, 1.0, 0.5)
                        .expect("instant start"),
                ),
            },
        ],
    );
    let observed = binding(&mut ends);
    send(
        &mut ends,
        at(1),
        vec![part(observed, DeckFilterChange::Install(Box::new(effect)))],
    );
    render_frames(&mut mixer, 1, 1);
    assert_eq!(receipts(&mut ends).len(), 2);
    tick(&mut reference);
    let before = ends.deck.snapshot.read().filter;
    let origin = 2;
    let mut knots = Vec::new();
    for cycle in 0..CYCLES {
        let start = origin + cycle * PERIOD;
        knots
            .push(ParameterKnot::new(frame_at(start), 20.0, CurveShape::HOLD).expect("low cutoff"));
        knots.push(
            ParameterKnot::new(frame_at(start + PERIOD - 1), 11_025.0, CurveShape::HOLD)
                .expect("high cutoff"),
        );
    }
    assert_eq!(knots.len(), FilterProgram::MAX_KNOTS);
    let program =
        FilterProgram::new(FilterParameter::CutoffHz, knots).expect("legal parameter points");
    let command = program_part(&mut ends, program);
    let seq = send(&mut ends, at(origin), vec![command]);
    let first = render_frames(&mut mixer, origin, 1);
    let replies = receipts(&mut ends);
    if !replies.is_empty() {
        assert_eq!(replies.len(), 1);
        assert!(matches!(
            outcome_of(&replies, seq),
            Outcome::Rejected(Rejection::Refused(DeckRefusal::Filter(
                kithara_effects::filter::FilterError::Parameters
            )))
        ));
        let after = ends.deck.snapshot.read().filter;
        assert_eq!(after.settings(), before.settings());
        assert_eq!(after.binding(), before.binding());
        assert_eq!(after.program(FilterParameter::CutoffHz), None);
        assert_eq!(
            after.authority(FilterParameter::CutoffHz),
            before.authority(FilterParameter::CutoffHz)
        );
        assert_eq!(bits(&first), bits(&[tick(&mut reference)]));
        return;
    }
    assert_eq!(
        ends.deck
            .snapshot
            .read()
            .filter
            .program(FilterParameter::CutoffHz),
        Some(seq)
    );
    assert!(first.iter().all(|sample| sample.is_finite()));
    // Stable poles at each endpoint do not establish stability of their switched product.
    for offset in (1..TAKE).step_by(BLOCK) {
        let pcm = render_frames(&mut mixer, origin + offset, BLOCK.min(TAKE - offset));
        assert!(
            pcm.iter().all(|sample| sample.is_finite()),
            "admitted normalized trajectory emitted nonfinite PCM at offset{offset}"
        );
    }
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, seq), origin);
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.metrics.underruns(), 0);
    assert_eq!(
        snapshot.slots[A.index()].state,
        SlotState::Playing,
        "an accepted trajectory cannot hide nonfinite processing behind terminal silence"
    );
}
