use kithara_command::Target;
use kithara_effects::filter::{FilterEffect, FilterKnob};

use super::{
    filter::{binding, bits, knob, prepared, reference, turn},
    *,
};
use crate::bridge::{CurveShape, FilterProgram, ParameterKnot};

fn linear(start: usize, end: usize, left: f64, right: f64) -> FilterProgram {
    FilterProgram::new(vec![
        ParameterKnot::new(frame_at(start), left, CurveShape::LINEAR).expect("left"),
        ParameterKnot::new(frame_at(end), right, CurveShape::HOLD).expect("right"),
    ])
    .expect("program")
}

fn program_part(ends: &mut TestEnds, program: FilterProgram) -> DeckPart {
    let snapshot = ends.deck.snapshot.read();
    DeckPart::FilterProgram {
        binding: snapshot.filter.binding().expect("observed output stream"),
        authority: snapshot.filter.authority(),
        program,
    }
}

/// The knob of a linear segment on frame `frame`, as the program computes it.
fn position(frame: usize, start: usize, end: usize, left: f64, right: f64) -> f64 {
    if frame >= end {
        return right;
    }
    let elapsed = (frame - start).to_f64().expect("frame offset");
    let duration = (end - start).to_f64().expect("segment length");
    left + (right - left) * (elapsed / duration)
}

/// Runs `knobs` through `filter` on a constant input, one value per frame.
fn programmed(filter: &mut FilterEffect, knobs: &[f64]) -> Vec<f32> {
    let mut planes = [vec![1.0; knobs.len()], vec![1.0; knobs.len()]];
    filter
        .process_program(&mut planes, 0..knobs.len(), knobs)
        .expect("reference program");
    assert_eq!(bits(&planes[0]), bits(&planes[1]));
    planes[0].clone()
}

#[kithara::test]
#[case::frame(1)]
#[case::odd(3)]
#[case::prime(7)]
#[case::whole(32)]
fn a_knob_program_is_frame_exact_in_any_block_partition(#[case] block: usize) {
    let (mut mixer, mut ends) = prepared();
    let origin = BLOCK;
    let sweep = program_part(&mut ends, linear(origin + 5, origin + 21, -0.2, -0.8));
    let seq = send(&mut ends, at(origin + 5), vec![sweep]);
    let late = program_part(&mut ends, linear(origin + 5, origin + 9, 0.5, 0.6));
    let refused_clock = send(&mut ends, at(origin + 6), vec![late]);
    let mut actual = Vec::new();
    for offset in (0..32).step_by(block) {
        actual.extend(render_frames(
            &mut mixer,
            origin + offset,
            block.min(32 - offset),
        ));
    }
    let knobs: Vec<_> = (5..32)
        .map(|frame| position(frame, 5, 21, -0.2, -0.8))
        .collect();
    let mut filter = reference(shape().sample_rate, FilterKnob::DEFAULT);
    let mut expected = vec![1.0; 5];
    expected.extend(programmed(&mut filter, &knobs));
    assert_eq!(bits(&actual), bits(&expected));
    let replies = receipts(&mut ends);
    assert_eq!(replies.len(), 2);
    assert_applied_at(outcome_of(&replies, seq), origin + 5);
    assert!(matches!(
        outcome_of(&replies, refused_clock),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::FilterProgramClock))
    ));
    let returned = &replies
        .iter()
        .find(|(returned, _, _)| *returned == seq)
        .expect("sweep receipt")
        .2;
    assert!(
        matches!(&returned[..], [DeckPart::FilterProgram { program, .. }] if program.start() == frame_at(origin + 5) && program.end() == frame_at(origin + 21))
    );
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.filter.program(), Some(seq));
    assert_eq!(snapshot.filter.authority(), None);
    assert_eq!(snapshot.filter.knob(), -0.8);
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn a_manual_turn_takes_over_from_the_last_applied_position() {
    let mut outputs = Vec::new();
    for manual_first in [false, true] {
        let (mut mixer, mut ends) = prepared();
        let origin = BLOCK;
        let sweep = program_part(&mut ends, linear(origin, origin + 16, -0.2, -0.8));
        let seq = send(&mut ends, at(origin), vec![sweep]);
        let mut actual = render_frames(&mut mixer, origin, 8);
        let replacement = program_part(&mut ends, linear(origin + 10, origin + 12, 1.0, 1.0));
        let manual = turn(0.5);
        let (manual_seq, replacement_seq) = if manual_first {
            let manual = send(&mut ends, at(origin + 10), vec![manual]);
            (manual, send(&mut ends, at(origin + 10), vec![replacement]))
        } else {
            let replacement = send(&mut ends, at(origin + 10), vec![replacement]);
            (send(&mut ends, at(origin + 10), vec![manual]), replacement)
        };
        actual.extend(render_frames(&mut mixer, origin + 8, 24));
        let knobs: Vec<_> = (0..10)
            .map(|frame| position(frame, 0, 16, -0.2, -0.8))
            .collect();
        let mut filter = reference(shape().sample_rate, FilterKnob::DEFAULT);
        let mut expected = programmed(&mut filter, &knobs);
        filter.configure(knob(0.5));
        let mut planes = [vec![1.0; 22], vec![1.0; 22]];
        filter.process(&mut planes, 0..22).expect("manual glide");
        expected.extend_from_slice(&planes[0]);
        assert_eq!(bits(&actual), bits(&expected));
        let replies = receipts(&mut ends);
        assert_applied_at(outcome_of(&replies, seq), origin);
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
        assert_eq!(snapshot.filter.authority(), Some(manual_seq));
        assert_eq!(snapshot.filter.program(), None);
        assert_eq!(snapshot.filter.knob().to_bits(), filter.knob().to_bits());
        assert_eq!(binding(&mut ends).instance(), None);
        outputs.push(actual);
    }
    assert_eq!(
        outputs.first().map(|pcm| bits(pcm)),
        outputs.last().map(|pcm| bits(pcm))
    );
}

/// A program that slams the knob between both ends for its whole length is
/// admitted and keeps normalized PCM finite.
#[kithara::test]
fn a_program_switching_between_the_knob_ends_is_admitted_and_stays_finite() {
    const PERIOD: usize = 552;
    const CYCLES: usize = FilterProgram::MAX_KNOTS / 2;
    const TAKE: usize = PERIOD * CYCLES;
    let (mut mixer, mut ends) = mixer_with_config(DeckMixerConfig::default(), 32);
    render_frames(&mut mixer, 0, 1);
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
    render_frames(&mut mixer, 1, 1);
    assert_eq!(receipts(&mut ends).len(), 1);
    let origin = 2;
    let mut knots = Vec::new();
    for cycle in 0..CYCLES {
        let start = origin + cycle * PERIOD;
        knots.push(ParameterKnot::new(frame_at(start), -1.0, CurveShape::HOLD).expect("low end"));
        knots.push(
            ParameterKnot::new(frame_at(start + PERIOD - 1), 1.0, CurveShape::HOLD)
                .expect("high end"),
        );
    }
    let program = FilterProgram::new(knots).expect("knob positions");
    let command = program_part(&mut ends, program);
    let seq = send(&mut ends, at(origin), vec![command]);
    for offset in (0..TAKE).step_by(BLOCK) {
        let pcm = render_frames(&mut mixer, origin + offset, BLOCK.min(TAKE - offset));
        assert!(
            pcm.iter().all(|sample| sample.is_finite()),
            "nonfinite PCM at offset {offset}"
        );
    }
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, seq), origin);
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.metrics.underruns(), 0);
    assert_eq!(snapshot.slots[A.index()].state, SlotState::Playing);
}
