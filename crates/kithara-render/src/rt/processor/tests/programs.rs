use super::*;
use crate::bridge::{
    CurveShape, DeckEqChange, DeckFxChange, DeckParameter, DeckPart, DeckProgram, DeckRefusal,
    LayoutRevision, ParameterKnot, Returned,
};

fn program(
    parameter: DeckParameter,
    start: usize,
    end: usize,
    left: f64,
    right: f64,
) -> DeckProgram {
    DeckProgram::new(
        parameter,
        vec![
            ParameterKnot::new(frame_at(start), left, CurveShape::LINEAR).expect("left"),
            ParameterKnot::new(frame_at(end), right, CurveShape::HOLD).expect("right"),
        ],
    )
    .expect("native program")
}

fn part(ends: &mut TestEnds, program: DeckProgram) -> DeckPart {
    let snapshot = ends.deck.snapshot.read();
    DeckPart::Program {
        epoch: snapshot.epoch.expect("live axis"),
        authority: snapshot
            .parameter(program.parameter())
            .and_then(|field| field.authority()),
        program,
    }
}

#[kithara::test]
fn every_channel_snapshot_reports_the_value_and_owner_of_its_audio_frame() {
    let (mut mixer, mut ends) =
        mixer_with_config(DeckMixerConfig::builder().eq_bands(0).build(), 64);
    start_dc(&mut mixer, &mut ends, &[A]);
    let targets = [
        DeckParameter::Trim,
        DeckParameter::Volume,
        DeckParameter::Crossfader,
        DeckParameter::Filter,
    ];
    let commands = targets
        .into_iter()
        .map(|target| {
            let (left, right) = if target == DeckParameter::Filter {
                (0.0, 0.0)
            } else {
                (1.0, 0.5)
            };
            part(&mut ends, program(target, BLOCK, BLOCK + 16, left, right))
        })
        .collect();
    let seq = send(&mut ends, at(BLOCK), commands);
    for frame in 0..=16_u32 {
        let value = 1.0 - 0.5 * f64::from(frame) / 16.0;
        let native = value.to_f32().expect("native value");
        let pcm = render_frames(
            &mut mixer,
            BLOCK + usize::try_from(frame).expect("frame"),
            1,
        );
        assert_eq!(pcm, [native * ((native * native) * native)]);
        let snapshot = ends.deck.snapshot.read();
        assert_eq!(
            snapshot.at,
            frame_at(BLOCK + usize::try_from(frame).expect("frame") + 1)
        );
        for target in targets {
            let field = snapshot.parameter(target).expect("prepared parameter");
            assert_eq!(
                field.value(),
                if target == DeckParameter::Filter {
                    0.0
                } else {
                    f64::from(native)
                }
            );
            assert_eq!(field.program(), Some(seq));
            assert_eq!(field.authority(), None);
        }
    }
    let returned = receipts(&mut ends);
    assert_applied_at(outcome_of(&returned, seq), BLOCK);
    assert_eq!(
        returned[0]
            .2
            .iter()
            .filter(|part| matches!(part, DeckPart::Returned(Returned::Program(_))))
            .count(),
        targets.len()
    );
}

#[kithara::test]
fn layout_and_unit_revisions_leave_other_program_scopes_running_and_refuse_stale_batches() {
    let (mut mixer, mut ends) =
        mixer_with_config(DeckMixerConfig::builder().eq_bands(1).build(), 64);
    start_dc(&mut mixer, &mut ends, &[A]);
    let layout = ends.deck.snapshot.read().eq.layout().expect("EQ layout");
    let eq = part(
        &mut ends,
        program(
            DeckParameter::EqBand { band: 0, layout },
            BLOCK,
            BLOCK + 64,
            0.0,
            -12.0,
        ),
    );
    let eq_seq = send(&mut ends, at(BLOCK), vec![eq]);
    let crossfader = part(
        &mut ends,
        program(DeckParameter::Crossfader, BLOCK, BLOCK + 64, 1.0, 0.5),
    );
    let cross_seq = send(&mut ends, at(BLOCK), vec![crossfader]);
    let stale = part(
        &mut ends,
        program(
            DeckParameter::EqBand { band: 0, layout },
            BLOCK + 20,
            BLOCK + 24,
            -24.0,
            -24.0,
        ),
    );
    let volume = part(
        &mut ends,
        program(DeckParameter::Volume, BLOCK + 20, BLOCK + 24, 0.25, 0.25),
    );
    let rejected = send(&mut ends, at(BLOCK + 20), vec![volume, stale]);
    let replaced = send(
        &mut ends,
        at(BLOCK + 8),
        vec![DeckPart::Eq(DeckEqChange::Layout(eq_layout(&[
            GainDb::DEFAULT,
        ])))],
    );
    let cleared = send(
        &mut ends,
        at(BLOCK + 10),
        vec![DeckPart::Fx(DeckFxChange::Clear { unit: 0 })],
    );
    render_frames(&mut mixer, BLOCK, 21);
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, eq_seq), BLOCK);
    assert_applied_at(outcome_of(&replies, replaced), BLOCK + 8);
    assert_applied_at(outcome_of(&replies, cleared), BLOCK + 10);
    assert!(matches!(
        outcome_of(&replies, rejected),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::ParameterOutdated {
            parameter: DeckParameter::EqBand { band: 0, .. }
        }))
    ));
    let snapshot = ends.deck.snapshot.read();
    assert_ne!(snapshot.eq.layout(), Some(layout));
    assert_eq!(snapshot.eq.program(0), None);
    let cross = snapshot
        .parameter(DeckParameter::Crossfader)
        .expect("crossfader");
    assert_eq!(cross.program(), Some(cross_seq));
    assert_eq!(cross.value(), 1.0 - 0.5 * 20.0 / 64.0);
    let volume = snapshot.parameter(DeckParameter::Volume).expect("volume");
    assert_eq!(volume.value(), 1.0);
    assert_eq!(volume.program(), None);
    assert_eq!(
        snapshot.fx_layouts[0].expect("unit 0").instance(),
        Some(cleared)
    );
    assert_eq!(snapshot.fx_layouts[1].expect("unit 1").instance(), None);
    let new_layout: LayoutRevision = snapshot.eq.layout().expect("new layout");
    let bad = part(
        &mut ends,
        program(
            DeckParameter::EqBand {
                band: 99,
                layout: new_layout,
            },
            BLOCK + 21,
            BLOCK + 25,
            -12.0,
            0.0,
        ),
    );
    let trim = part(
        &mut ends,
        program(DeckParameter::Trim, BLOCK + 21, BLOCK + 25, 0.25, 0.25),
    );
    let unknown = send(&mut ends, at(BLOCK + 21), vec![trim, bad]);
    render_frames(&mut mixer, BLOCK + 21, 1);
    let replies = receipts(&mut ends);
    assert!(matches!(
        outcome_of(&replies, unknown),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::ParameterUnavailable {
            parameter: DeckParameter::EqBand { band: 99, .. }
        }))
    ));
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(
        snapshot
            .parameter(DeckParameter::Trim)
            .expect("trim")
            .value(),
        1.0
    );
    assert_eq!(
        snapshot
            .parameter(DeckParameter::Crossfader)
            .expect("crossfader")
            .program(),
        Some(cross_seq)
    );
}
