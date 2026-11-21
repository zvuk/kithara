use super::*;
use crate::bridge::{CurveShape, DeckParameter, DeckProgram, LayoutRevision, ParameterKnot};

fn prepared(bands: usize) -> (TestMixer, TestEnds) {
    prepared_with_capacity(bands, 64)
}

fn prepared_with_capacity(bands: usize, capacity: usize) -> (TestMixer, TestEnds) {
    let config = DeckMixerConfig::builder().eq_bands(bands).build();
    let (mut mixer, mut ends) = mixer_with_config(config, capacity);
    start_dc(&mut mixer, &mut ends, &[A]);
    (mixer, ends)
}

fn program(
    band: usize,
    start: usize,
    end: usize,
    shape: CurveShape,
) -> impl FnOnce(LayoutRevision) -> DeckProgram {
    eq_program(
        band,
        vec![
            ParameterKnot::new(frame_at(start), -12.0, shape).expect("left gain"),
            ParameterKnot::new(frame_at(end), 0.0, CurveShape::HOLD).expect("right gain"),
        ],
    )
}

fn eq_program(
    band: usize,
    knots: Vec<ParameterKnot>,
) -> impl FnOnce(LayoutRevision) -> DeckProgram {
    move |layout| {
        DeckProgram::new(DeckParameter::EqBand { band, layout }, knots)
            .expect("prepared EQ program")
    }
}

fn part(ends: &mut TestEnds, prepare: impl FnOnce(LayoutRevision) -> DeckProgram) -> DeckPart {
    let snapshot = ends.deck.snapshot.read();
    let program = prepare(snapshot.eq.layout().expect("observed EQ layout"));
    DeckPart::Program {
        epoch: snapshot.epoch.expect("observed EQ axis"),
        authority: snapshot.eq.authority(match program.parameter() {
            DeckParameter::EqBand { band, .. } => band,
            _ => unreachable!(),
        }),
        program,
    }
}

fn value_at(curve: &crate::bridge::ParameterCurve, at: SessionFrame) -> f64 {
    let mut value = [0.0];
    curve.write(
        &mut crate::bridge::CurveCursor::default(),
        at,
        &mut value,
        false,
    );
    value[0]
}

fn amplitude(gain: GainDb) -> f32 {
    if gain == GainDb::MIN {
        0.0
    } else {
        10.0_f32.powf(f32::from(gain) / 20.0)
    }
}

#[kithara::test]
#[case(0)]
#[case(5)]
fn eq_kill_is_timed_and_preserves_the_running_program_and_authority(#[case] offset: usize) {
    let (mut mixer, mut ends) = prepared(1);
    let command = part(&mut ends, program(0, BLOCK, BLOCK + 40, CurveShape::HOLD));
    let seq = send(&mut ends, at(BLOCK), vec![command]);
    let before = render_frames(&mut mixer, BLOCK, 8);
    let gain = GainDb::checked(-12.0).expect("prepared gain");
    assert!(
        before
            .iter()
            .all(|sample| sample.to_bits() == amplitude(gain).to_bits())
    );
    let (layout, authority) = {
        let snapshot = ends.deck.snapshot.read();
        (
            snapshot.eq.layout().expect("layout"),
            snapshot.eq.authority(0),
        )
    };
    let on = BLOCK + 8 + offset;
    let off = on + 8;
    let kill = |on| {
        DeckPart::Eq(DeckEqChange::Kill {
            band: 0,
            layout,
            on,
        })
    };
    let killed = send(&mut ends, at(on), vec![kill(true)]);
    let restored = send(&mut ends, at(off), vec![kill(false)]);
    let pcm = render_frames(&mut mixer, BLOCK + 8, offset + 8);
    assert!(
        pcm[..offset]
            .iter()
            .all(|value| value.to_bits() == amplitude(gain).to_bits())
    );
    assert!((0.0..amplitude(gain)).contains(&pcm[offset]));
    {
        let snapshot = ends.deck.snapshot.read();
        assert_eq!(snapshot.at, frame_at(off));
        assert_eq!(snapshot.eq.killed(0), Some(true));
        assert_eq!(snapshot.eq.gain(0), Some(gain));
        assert_eq!(snapshot.eq.authority(0), authority);
        assert_eq!(snapshot.eq.program(0), Some(seq));
    }
    let restore = render_frames(&mut mixer, off, 1);
    assert!(restore[0] > *pcm.last().expect("last masked frame"));
    assert!(restore[0] < amplitude(gain));
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.eq.killed(0), Some(false));
    assert_eq!(snapshot.eq.gain(0), Some(gain));
    assert_eq!(snapshot.eq.authority(0), authority);
    assert_eq!(snapshot.eq.program(0), Some(seq));
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, killed), on);
    assert_applied_at(outcome_of(&replies, restored), off);
}

#[kithara::test]
fn eq_kill_refuses_a_stale_layout_or_unknown_band_before_any_batch_changes() {
    let (mut mixer, mut ends) = prepared(2);
    let revision = ends.deck.snapshot.read().eq.layout().expect("layout");
    let kill = |band| {
        DeckPart::Eq(DeckEqChange::Kill {
            band,
            layout: revision,
            on: true,
        })
    };
    let layout = || DeckPart::Eq(DeckEqChange::Layout(eq_layout(&[GainDb::MAX])));
    let same_batch = send(&mut ends, at(BLOCK), vec![layout(), kill(0)]);
    let invalid = send(&mut ends, at(BLOCK + 1), vec![kill(0), kill(2)]);
    let stale = send(&mut ends, at(BLOCK + 4), vec![kill(0)]);
    render_frames(&mut mixer, BLOCK, 2);
    assert_eq!(ends.deck.snapshot.read().eq.layout(), Some(revision));
    assert_eq!(ends.deck.snapshot.read().eq.killed(0), Some(false));
    assert_eq!(ends.deck.snapshot.read().eq.gain(0), Some(GainDb::DEFAULT));
    let replacement = send(&mut ends, at(BLOCK + 2), vec![layout()]);
    render_frames(&mut mixer, BLOCK + 2, 4);
    let replies = receipts(&mut ends);
    for seq in [same_batch, stale] {
        assert!(matches!(
            outcome_of(&replies, seq),
            Outcome::Rejected(Rejection::Refused(DeckRefusal::ParameterOutdated {
                parameter: DeckParameter::EqBand { band: 0, .. }
            }))
        ));
    }
    assert!(matches!(
        outcome_of(&replies, invalid),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::ParameterUnavailable {
            parameter: DeckParameter::EqBand { band: 2, .. }
        }))
    ));
    assert_applied_at(outcome_of(&replies, replacement), BLOCK + 2);
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.eq.killed(0), Some(false));
    assert_eq!(snapshot.eq.killed(1), None);
}

#[kithara::test]
fn eq_kill_reset_neutralizes_the_addressed_bands_and_keeps_existing_takeover() {
    let (mut mixer, mut ends) = prepared(2);
    let command = part(&mut ends, program(0, BLOCK, BLOCK + 64, CurveShape::HOLD));
    let program = send(&mut ends, at(BLOCK), vec![command]);
    let layout = ends.deck.snapshot.read().eq.layout().expect("layout");
    send(
        &mut ends,
        at(BLOCK + 1),
        vec![DeckPart::Eq(DeckEqChange::Kill {
            band: 0,
            layout,
            on: true,
        })],
    );
    render_frames(&mut mixer, BLOCK, 2);
    assert_eq!(ends.deck.snapshot.read().eq.killed(0), Some(true));
    assert_eq!(ends.deck.snapshot.read().eq.program(0), Some(program));

    let reset = send(
        &mut ends,
        at(BLOCK + 2),
        vec![DeckPart::Eq(DeckEqChange::Reset)],
    );
    render_frames(&mut mixer, BLOCK + 2, 1);
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.eq.killed(0), Some(false));
    assert_eq!(snapshot.eq.gain(0), Some(GainDb::DEFAULT));
    assert_eq!(snapshot.eq.program(0), None);
    assert_eq!(snapshot.eq.authority(0), Some(reset));
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, reset), BLOCK + 2);
}

#[kithara::test]
fn eq_curves_apply_each_frame_without_second_smoothing_or_partition_drift() {
    type Oracle = fn(f64) -> f64;
    let fixtures: [(CurveShape, Oracle); 3] = [
        (CurveShape::HOLD, |_| 0.0),
        (CurveShape::LINEAR, |t| t),
        (CurveShape::cubic(0.0, 1.0).expect("S shape"), |t| {
            3.0 * t * t - 2.0 * t * t * t
        }),
    ];
    for (shape, progression) in fixtures {
        for block in [1_usize, 3, 7, 32] {
            let (mut mixer, mut ends) = prepared(1);
            let command = part(&mut ends, program(0, BLOCK + 5, BLOCK + 21, shape));
            let seq = send(&mut ends, at(BLOCK + 5), vec![command]);
            assert_eq!(ends.deck.snapshot.read().eq.gain(0), Some(GainDb::DEFAULT));
            assert_eq!(ends.deck.snapshot.read().eq.program(0), None);
            let mut pcm = Vec::new();
            for offset in (0..32).step_by(block) {
                pcm.extend(render_frames(
                    &mut mixer,
                    BLOCK.saturating_add(offset),
                    block.min(32_usize.saturating_sub(offset)),
                ));
            }
            let oracle: Vec<_> = (0_u32..32)
                .map(|frame| {
                    if !(5..21).contains(&frame) {
                        return 1.0;
                    }
                    let t = f64::from(frame.saturating_sub(5)) / 16.0;
                    let db = -12.0 + 12.0 * progression(t);
                    amplitude(
                        GainDb::checked(db.to_f32().expect("bounded dB")).expect("admitted dB"),
                    )
                })
                .collect();
            assert_eq!(pcm, oracle);
            assert_ne!(pcm, vec![1.0; 32], "unchanged PCM must fail the oracle");
            assert_applied_at(outcome_of(&receipts(&mut ends), seq), BLOCK + 5);
            let snapshot = ends.deck.snapshot.read();
            assert_eq!(snapshot.eq.gain(0), Some(GainDb::DEFAULT));
            assert_eq!(snapshot.eq.program(0), Some(seq));
            assert_eq!(snapshot.metrics.underruns(), 0);
        }
    }
}

#[kithara::test]
fn same_frame_manual_control_does_not_start_from_an_unrendered_replacement() {
    let mut outputs = Vec::new();
    for manual_first in [false, true] {
        let (mut mixer, mut ends) = prepared(1);
        let command = part(
            &mut ends,
            program(0, BLOCK + 2, BLOCK + 18, CurveShape::LINEAR),
        );
        let last_program_gain = match &command {
            DeckPart::Program { program, .. } => GainDb::checked(
                value_at(program.curve(), frame_at((BLOCK + 10).saturating_sub(1)))
                    .to_f32()
                    .expect("native program gain"),
            )
            .expect("declared EQ range"),
            _ => unreachable!("prepared EQ program"),
        };
        send(&mut ends, at(BLOCK + 2), vec![command]);
        render_frames(&mut mixer, BLOCK, 8);
        let replacement = eq_program(
            0,
            vec![ParameterKnot::new(frame_at(BLOCK + 10), -24.0, CurveShape::HOLD).expect("kill")],
        );
        let replacement = part(&mut ends, replacement);
        let manual = DeckPart::Eq(DeckEqChange::Gain {
            band: 0,
            gain: GainDb::MAX,
        });
        let (manual_seq, replacement_seq) = if manual_first {
            let manual = send(&mut ends, at(BLOCK + 10), vec![manual]);
            (manual, send(&mut ends, at(BLOCK + 10), vec![replacement]))
        } else {
            let replacement = send(&mut ends, at(BLOCK + 10), vec![replacement]);
            (send(&mut ends, at(BLOCK + 10), vec![manual]), replacement)
        };
        let pcm = render_frames(&mut mixer, BLOCK + 8, 24);
        let smoothing = EqConfig::builder(pools()).build().smoothing();
        let coeff = kithara_dsp::param::SmoothingFilterCoeff::new(
            shape().sample_rate,
            smoothing.smooth_seconds,
            smoothing.settle_ratio,
        );
        let last_applied = amplitude(last_program_gain);
        let mut reference = kithara_dsp::param::SmoothingFilter::new(last_applied);
        reference.process(amplitude(GainDb::MAX), coeff);
        reference.try_settle(
            amplitude(GainDb::MAX),
            kithara_dsp::param::DEFAULT_GAIN_SPAN,
            smoothing.settle_ratio,
        );
        assert_eq!(pcm.get(2).copied(), Some(reference.z1));
        let before_manual = pcm.get(1).copied().expect("last programme frame");
        let first_manual = pcm.get(2).copied().expect("takeover frame");
        assert_eq!(
            before_manual, last_applied,
            "the origin is the last rendered programme value"
        );
        assert!(
            (first_manual - before_manual).abs() <= (reference.z1 - last_applied).abs(),
            "the takeover seam moves by at most one smoothing step"
        );
        let replies = receipts(&mut ends);
        assert_applied_at(outcome_of(&replies, manual_seq), BLOCK + 10);
        if manual_first {
            assert!(matches!(
                outcome_of(&replies, replacement_seq),
                Outcome::Rejected(Rejection::Refused(DeckRefusal::ParameterOutdated {
                    parameter: DeckParameter::EqBand { band: 0, .. }
                }))
            ));
        } else {
            assert_applied_at(outcome_of(&replies, replacement_seq), BLOCK + 10);
        }
        let returned = replies
            .iter()
            .find(|(seq, _, _)| *seq == replacement_seq)
            .and_then(|(_, _, parts)| {
                parts.iter().find_map(|part| match part {
                    DeckPart::Program { program, .. }
                    | DeckPart::Returned(Returned::Program(program)) => Some(program),
                    _ => None,
                })
            })
            .expect("replacement storage returned");
        assert_eq!(returned.start(), frame_at(BLOCK + 10));
        assert_eq!(value_at(returned.curve(), returned.start()), -24.0);
        let snapshot = ends.deck.snapshot.read();
        assert_eq!(snapshot.eq.authority(0), Some(manual_seq));
        assert_eq!(snapshot.eq.program(0), None);
        assert_eq!(snapshot.eq.gain(0), Some(GainDb::MAX));
        assert_eq!(snapshot.metrics.underruns(), 0);
        outputs.push(pcm);
    }
    assert_eq!(outputs.first(), outputs.last());
}

#[kithara::test]
fn every_eq_parameter_has_its_own_program_slot() {
    let (mut mixer, mut ends) = prepared(17);
    let mut accepted = Vec::new();
    for band in 0..16 {
        let command = part(
            &mut ends,
            program(band, BLOCK, BLOCK + 1000, CurveShape::LINEAR),
        );
        accepted.push(send(&mut ends, at(BLOCK), vec![command]));
    }
    render_frames(&mut mixer, BLOCK, 1);
    assert!(
        receipts(&mut ends).is_empty(),
        "running fields retain their knots"
    );
    let excess = part(
        &mut ends,
        program(16, BLOCK + 1, BLOCK + 1001, CurveShape::LINEAR),
    );
    let refused = send(&mut ends, at(BLOCK + 1), vec![excess]);
    render_frames(&mut mixer, BLOCK + 1, 1);
    let replies = receipts(&mut ends);
    assert!(
        replies.is_empty(),
        "the seventeenth field is admitted independently"
    );
    let snapshot = ends.deck.snapshot.read();
    for (band, seq) in accepted.into_iter().enumerate() {
        assert_eq!(snapshot.eq.program(band), Some(seq));
    }
    assert_eq!(snapshot.eq.program(16), Some(refused));
    assert_eq!(snapshot.eq.gain(16), GainDb::checked(-12.0));
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn a_future_curve_cannot_control_an_eq_layout_that_replaced_its_binding() {
    let (mut mixer, mut ends) = prepared(1);
    let kill = eq_program(
        0,
        vec![ParameterKnot::new(frame_at(BLOCK + 20), -24.0, CurveShape::HOLD).expect("kill")],
    );
    let stale = part(&mut ends, kill);
    let stale = send(&mut ends, at(BLOCK + 20), vec![stale]);
    send(
        &mut ends,
        at(BLOCK + 2),
        vec![DeckPart::Eq(DeckEqChange::Layout(eq_layout(&[])))],
    );
    send(
        &mut ends,
        at(BLOCK + 3),
        vec![DeckPart::Eq(DeckEqChange::Layout(eq_layout(&[
            GainDb::DEFAULT,
        ])))],
    );
    let pcm = render_frames(&mut mixer, BLOCK, 32);
    let config = EqConfig::builder(pools()).build();
    let mut reference = kithara_effects::eq::StereoEq::with_layout(
        &config,
        shape().sample_rate,
        eq_layout(&[GainDb::DEFAULT]),
    );
    let expected: Vec<_> = (0..32)
        .map(|frame| {
            if frame == 2 {
                reference.take_layout(eq_layout(&[]));
            }
            if frame == 3 {
                reference.take_layout(eq_layout(&[GainDb::DEFAULT]));
            }
            let (mut left, mut right) = ([1.0], [1.0]);
            reference.process(&mut left, &mut right);
            left[0].to_bits()
        })
        .collect();
    assert!(expected.iter().any(|sample| *sample != 1.0_f32.to_bits()));
    assert_eq!(
        pcm.iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        expected
    );
    let replies = receipts(&mut ends);
    assert!(matches!(
        outcome_of(&replies, stale),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::ParameterOutdated {
            parameter: DeckParameter::EqBand { band: 0, .. }
        }))
    ));
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.eq.gain(0), Some(GainDb::DEFAULT));
    assert_eq!(snapshot.eq.program(0), None);
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn a_refused_band_preserves_the_running_eq_curve_and_returns_storage() {
    let (mut mixer, mut ends) = prepared(1);
    let command = part(&mut ends, program(0, BLOCK, BLOCK + 16, CurveShape::LINEAR));
    let running = send(&mut ends, at(BLOCK), vec![command]);
    render_frames(&mut mixer, BLOCK, 8);
    let bad = part(
        &mut ends,
        program(99, BLOCK + 8, BLOCK + 24, CurveShape::HOLD),
    );
    let refused = send(&mut ends, at(BLOCK + 8), vec![bad]);
    let pcm = render_frames(&mut mixer, BLOCK + 8, 8);
    let expected: Vec<_> = (8_u32..16)
        .map(|frame| {
            let db = -12.0 + 12.0 * f64::from(frame) / 16.0;
            amplitude(GainDb::checked(db.to_f32().expect("bounded dB")).expect("admitted dB"))
        })
        .collect();
    assert_eq!(pcm, expected);
    let replies = receipts(&mut ends);
    assert_eq!(replies.len(), 1);
    assert!(matches!(
        outcome_of(&replies, refused),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::ParameterUnavailable {
            parameter: DeckParameter::EqBand { band: 99, .. }
        }))
    ));
    assert!(matches!(
        replies.first().and_then(|(_, _, parts)| parts.first()),
        Some(DeckPart::Program { .. })
    ));
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.eq.program(0), Some(running));
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn completed_eq_programs_release_credit_before_the_next_admission() {
    let (mut mixer, mut ends) = prepared_with_capacity(1, 1);
    let mut start = BLOCK;
    for _ in 0..5 {
        let end = start.saturating_add(4);
        let command = part(&mut ends, program(0, start, end, CurveShape::LINEAR));
        let seq = send(&mut ends, at(start), vec![command]);
        render_frames(&mut mixer, start, 5);
        let replies = receipts(&mut ends);
        assert_eq!(replies.len(), 1);
        assert_applied_at(outcome_of(&replies, seq), start);
        assert!(matches!(
            replies.first().and_then(|(_, _, parts)| parts.first()),
            Some(DeckPart::Returned(Returned::Program(_)))
        ));
        assert_eq!(ends.deck.snapshot.read().eq.gain(0), Some(GainDb::DEFAULT));
        assert_eq!(ends.deck.snapshot.read().eq.program(0), Some(seq));
        start = end.saturating_add(1);
    }
    assert_eq!(ends.deck.snapshot.read().metrics.underruns(), 0);
}

#[kithara::test]
fn closing_the_parameter_owner_confirms_only_rendered_values_and_returns_knots() {
    let (mut mixer, mut ends) = prepared(1);
    let command = part(
        &mut ends,
        program(0, BLOCK, BLOCK + 128, CurveShape::LINEAR),
    );
    let seq = send(&mut ends, at(BLOCK), vec![command]);
    render_frames(&mut mixer, BLOCK, 8);
    assert!(receipts(&mut ends).is_empty());
    let scope = ends.deck.scope;
    ends.ring.close(scope).expect("close deck parameter owner");
    ends.ring.publish().expect("publish close");
    render_frames(&mut mixer, BLOCK + 8, 8);
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, seq), BLOCK);
    assert!(matches!(
        replies.first().and_then(|(_, _, parts)| parts.first()),
        Some(DeckPart::Returned(Returned::Program(_)))
    ));
    let snapshot = ends.deck.snapshot.read();
    let last = GainDb::checked(-12.0 + 12.0 * 15.0 / 128.0).expect("last rendered gain");
    let returned = replies
        .iter()
        .find(|(answered, _, _)| *answered == seq)
        .and_then(|(_, _, parts)| {
            parts.iter().find_map(|part| match part {
                DeckPart::Returned(Returned::Program(program)) => Some(program),
                _ => None,
            })
        })
        .expect("terminal program storage");
    assert_eq!(returned.start(), frame_at(BLOCK));
    assert_eq!(returned.end(), frame_at(BLOCK + 128));
    assert_eq!(value_at(returned.curve(), returned.end()), 0.0);
    assert_eq!(snapshot.eq.gain(0), Some(last));
    assert_ne!(
        snapshot.eq.gain(0),
        Some(GainDb::DEFAULT),
        "unrendered endpoint is not confirmed"
    );
    assert_eq!(snapshot.eq.layout(), None);
    assert_eq!(snapshot.eq.program(0), None);
}

#[kithara::test]
fn manual_control_revokes_only_its_eq_field() {
    let (mut mixer, mut ends) = prepared(2);
    let mut programs = Vec::new();
    for band in 0..2 {
        let command = part(
            &mut ends,
            program(band, BLOCK, BLOCK + 64, CurveShape::LINEAR),
        );
        programs.push(send(&mut ends, at(BLOCK), vec![command]));
    }
    send(
        &mut ends,
        at(BLOCK + 8),
        vec![DeckPart::Eq(DeckEqChange::Gain {
            band: 0,
            gain: GainDb::MAX,
        })],
    );
    render_frames(&mut mixer, BLOCK, 32);
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.eq.program(0), None);
    assert_eq!(snapshot.eq.gain(0), Some(GainDb::MAX));
    assert_eq!(snapshot.eq.program(1), programs.get(1).copied());
    assert_eq!(
        snapshot.eq.gain(1),
        GainDb::checked(-12.0 + 12.0 * 31.0 / 64.0)
    );
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn future_eq_programs_do_not_shift_a_slots_resume_boundary() {
    let (mut mixer, mut ends) = prepared(1);
    let command = part(
        &mut ends,
        program(0, BLOCK + 32, BLOCK + 48, CurveShape::LINEAR),
    );
    send(&mut ends, at(BLOCK + 32), vec![command]);
    let stop = send(
        &mut ends,
        at(BLOCK + 4),
        vec![DeckPart::Stop {
            slot: A,
            fade: Fade::Crossfade(
                CrossfadeSettings::new(0.0, CrossfadeCurve::Linear, 1.0, 0.5)
                    .expect("instant stop"),
            ),
        }],
    );
    render_frames(&mut mixer, BLOCK, 64);
    assert_stopped_once(&receipts(&mut ends), stop, BLOCK + 4, A, BLOCK + 4);
    let snapshot = ends.deck.snapshot.read();
    assert!(
        snapshot.eq.program(0).is_some(),
        "the deck program survives the slot's stop"
    );
    assert_eq!(snapshot.eq.gain(0), Some(GainDb::DEFAULT));
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn mixed_shape_eq_knots_preserve_each_pcm_frame_across_render_subdivisions() {
    for block in [1, 7, 13, 128] {
        let (mut mixer, mut ends) = prepared(1);
        let curve = eq_program(
            0,
            [
                (17, 0.0, CurveShape::cubic(0.0, 1.0).expect("S shape")),
                (33, -12.0, CurveShape::HOLD),
                (40, -12.0, CurveShape::LINEAR),
                (48, 6.0, CurveShape::HOLD),
            ]
            .into_iter()
            .map(|(at, value, shape)| {
                ParameterKnot::new(frame_at(BLOCK + at), value, shape).expect("finite knot")
            })
            .collect(),
        );
        let curve = part(&mut ends, curve);
        let seq = send(&mut ends, at(BLOCK + 17), vec![curve]);
        let mut pcm = Vec::new();
        for offset in (0..96).step_by(block) {
            pcm.extend(render_frames(
                &mut mixer,
                BLOCK + offset,
                block.min(96 - offset),
            ));
        }
        let expected: Vec<_> = (0..96_u32)
            .map(|frame| {
                let db = match frame {
                    0..17 => 0.0,
                    17..33 => {
                        let t = f64::from(frame - 17) / 16.0;
                        -12.0 * (3.0 * t * t - 2.0 * t * t * t)
                    }
                    33..40 => -12.0,
                    40..48 => -12.0 + 18.0 * f64::from(frame - 40) / 8.0,
                    _ => 6.0,
                };
                amplitude(GainDb::checked(db.to_f32().expect("canonical dB")).expect("in range"))
            })
            .collect();
        assert_eq!(pcm, expected, "subdivision {block}");
        assert_applied_at(outcome_of(&receipts(&mut ends), seq), BLOCK + 17);
        let snapshot = ends.deck.snapshot.read();
        assert_eq!(snapshot.eq.gain(0), Some(GainDb::MAX));
        assert_eq!(snapshot.eq.program(0), Some(seq));
        assert_eq!(snapshot.metrics.underruns(), 0);
    }
}

#[kithara::test]
#[case::clock(0)]
#[case::epoch(1)]
#[case::mixed_batch(2)]
fn eq_refusals_preserve_running_pcm_and_return_the_original_program(#[case] kind: u8) {
    let (mut mixer, mut ends) = prepared(1);
    let running = part(&mut ends, program(0, BLOCK, BLOCK + 32, CurveShape::LINEAR));
    let running = send(&mut ends, at(BLOCK), vec![running]);
    render_frames(&mut mixer, BLOCK, 8);
    let band = if kind == 2 { 99 } else { 0 };
    let start = BLOCK + if kind == 0 { 9 } else { 8 };
    let mut replacement = part(
        &mut ends,
        program(band, start, BLOCK + 24, CurveShape::HOLD),
    );
    if let DeckPart::Program { epoch, .. } = &mut replacement
        && kind == 1
    {
        *epoch = SessionEpoch::new(1);
    }
    let mut commands = vec![replacement];
    if kind == 2 {
        commands.insert(
            0,
            DeckPart::Eq(DeckEqChange::Gain {
                band: 0,
                gain: GainDb::MIN,
            }),
        );
    }
    let rejected = send(&mut ends, at(BLOCK + 8), commands);
    let pcm = render_frames(&mut mixer, BLOCK + 8, 8);
    let expected: Vec<_> = (8..16_u32)
        .map(|frame| {
            amplitude(
                GainDb::checked(-12.0 + 12.0 * frame.to_f32().expect("local frame") / 32.0)
                    .expect("curve gain"),
            )
        })
        .collect();
    assert_eq!(pcm, expected);
    let replies = receipts(&mut ends);
    assert_eq!(replies.len(), 1);
    assert!(
        matches!(outcome_of(&replies, rejected), Outcome::Rejected(Rejection::Refused(found)) if match kind {
            0 => matches!(found, DeckRefusal::ProgramClock { parameter: DeckParameter::EqBand { band: 0, .. } }),
            1 => *found == DeckRefusal::AxisRestarted,
            _ => matches!(found, DeckRefusal::ParameterUnavailable { parameter: DeckParameter::EqBand { band: 99, .. } }),
        })
    );
    let returned = replies
        .iter()
        .find(|(seq, ..)| *seq == rejected)
        .and_then(|(_, _, parts)| {
            parts.iter().find_map(|part| match part {
                DeckPart::Program { program, .. } => Some(program),
                _ => None,
            })
        })
        .expect("refused programme storage returned");
    assert!(
        matches!(returned.parameter(), DeckParameter::EqBand { band: actual, .. } if actual == band)
    );
    assert_eq!(returned.start(), frame_at(start));
    assert_eq!(returned.end(), frame_at(BLOCK + 24));
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.eq.program(0), Some(running));
    assert_eq!(
        snapshot.eq.gain(0),
        GainDb::checked(-12.0 + 12.0 * 15.0 / 32.0)
    );
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn neutral_programs_on_every_band_leave_the_pcm_unchanged() {
    let (mut mixer, mut ends) = prepared(17);
    for band in 0..16 {
        let curve = eq_program(
            band,
            [BLOCK, BLOCK + 64]
                .into_iter()
                .map(|at| {
                    ParameterKnot::new(frame_at(at), 0.0, CurveShape::HOLD).expect("neutral knot")
                })
                .collect(),
        );
        let curve = part(&mut ends, curve);
        send(&mut ends, at(BLOCK), vec![curve]);
    }
    assert_eq!(render_frames(&mut mixer, BLOCK, 1), [1.0]);
    assert!(receipts(&mut ends).is_empty());
    let curve = eq_program(
        16,
        [BLOCK + 1, BLOCK + 65]
            .into_iter()
            .map(|at| {
                ParameterKnot::new(frame_at(at), 0.0, CurveShape::HOLD).expect("neutral knot")
            })
            .collect(),
    );
    let curve = part(&mut ends, curve);
    let excess = send(&mut ends, at(BLOCK + 1), vec![curve]);
    assert_eq!(render_frames(&mut mixer, BLOCK + 1, 1), [1.0]);
    assert!(
        receipts(&mut ends).is_empty(),
        "every neutral band program is admitted"
    );
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.eq.gain(16), Some(GainDb::DEFAULT));
    assert_eq!(snapshot.eq.program(16), Some(excess));
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn manual_eq_steps_keep_their_pcm_law_at_the_exact_deck_frame() {
    for block in [1, 13, 128] {
        let (mut mixer, mut ends) = prepared(1);
        let seq = send(
            &mut ends,
            at(BLOCK + 17),
            vec![DeckPart::Eq(DeckEqChange::Gain {
                band: 0,
                gain: GainDb::MIN,
            })],
        );
        assert_eq!(ends.deck.snapshot.read().eq.gain(0), Some(GainDb::DEFAULT));
        let config = EqConfig::builder(pools()).build();
        let mut reference = kithara_effects::eq::StereoEq::with_layout(
            &config,
            shape().sample_rate,
            eq_layout(&[GainDb::DEFAULT]),
        );
        let expected: Vec<_> = (0..96)
            .map(|frame| {
                if frame == 17 {
                    reference.set_gain(0, GainDb::MIN);
                }
                let (mut left, mut right) = ([1.0], [1.0]);
                reference.process(&mut left, &mut right);
                left[0].to_bits()
            })
            .collect();
        let mut pcm = Vec::new();
        for offset in (0..96).step_by(block) {
            pcm.extend(render_frames(
                &mut mixer,
                BLOCK + offset,
                block.min(96 - offset),
            ));
        }
        assert!(pcm[..17].iter().all(|sample| *sample == 1.0));
        assert_eq!(
            pcm.iter()
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>(),
            expected,
            "subdivision {block}"
        );
        assert_applied_at(outcome_of(&receipts(&mut ends), seq), BLOCK + 17);
        assert_eq!(ends.deck.snapshot.read().eq.gain(0), Some(GainDb::MIN));
        assert_eq!(ends.deck.snapshot.read().metrics.underruns(), 0);
    }
}
