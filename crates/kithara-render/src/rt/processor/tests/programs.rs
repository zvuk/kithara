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

#[kithara::test]
#[case::volume(DeckParameter::Volume)]
#[case::crossfader(DeckParameter::Crossfader)]
fn output_factor_ownership_is_continuous_from_manual_to_program_held_and_back(
    #[case] parameter: DeckParameter,
) {
    let half_volume = kithara_signal::FaderValue::checked(0.5).expect("half volume");
    let half_cross = crate::bridge::CrossfaderGain::checked(0.5).expect("half crossfader");
    let mix = crate::bridge::DeckMixSettings::builder()
        .volume(half_volume)
        .crossfader(half_cross)
        .build();
    let (mut mixer, mut ends) =
        mixer_with_config(DeckMixerConfig::builder().eq_bands(0).mix(mix).build(), 64);
    start_dc(&mut mixer, &mut ends, &[A]);
    let gain = mix.output_gain();
    let command = part(&mut ends, program(parameter, BLOCK, BLOCK + 8, 0.5, 0.5));
    let seq = send(&mut ends, at(BLOCK), vec![command]);
    let before = render_frames(&mut mixer, BLOCK, 10);
    assert!(
        before.iter().all(|value| value.to_bits() == gain.to_bits()),
        "entry and Held apply the factor exactly once"
    );
    assert_eq!(
        ends.deck
            .snapshot
            .read()
            .parameter(parameter)
            .expect("held parameter")
            .program(),
        Some(seq)
    );
    let change = if parameter == DeckParameter::Volume {
        crate::bridge::DeckMixSettingsChange::Volume(
            kithara_signal::FaderValue::checked(0.25).expect("manual volume"),
        )
    } else {
        crate::bridge::DeckMixSettingsChange::Crossfader(
            crate::bridge::CrossfaderGain::checked(0.25).expect("manual crossfader"),
        )
    };
    let mut target = mix;
    kithara_config::LiveConfig::apply_change(&mut target, change);
    let mut reference = kithara_dsp::param::SmoothedParam::new(
        gain,
        crate::consts::GAIN_SPAN,
        SmootherConfig::default(),
        shape().sample_rate,
    );
    reference.set_value(target.output_gain());
    let first = reference.next_smoothed();
    send(&mut ends, at(BLOCK + 10), vec![DeckPart::Mix(change)]);
    let after = render_frames(&mut mixer, BLOCK + 10, 1);
    assert_eq!(
        after[0].to_bits(),
        first.to_bits(),
        "manual starts from the applied combined gain"
    );
    let program_step = before
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max);
    let main_step = (first - gain).abs();
    assert!(
        (after[0] - before[9]).abs() <= program_step.max(main_step),
        "ownership transfer introduces no additional gain step"
    );
    assert_eq!(
        ends.deck
            .snapshot
            .read()
            .parameter(parameter)
            .expect("manual parameter")
            .program(),
        None
    );
}

#[kithara::test]
#[case::ordinary(0.5_f32)]
#[case::tiny(1.0e-20_f32)]
fn output_program_entry_snaps_overlapping_manual_ramps_to_remainder_targets(#[case] first: f32) {
    use crate::bridge::{CrossfaderGain, DeckMixSettingsChange};
    let (mut mixer, mut ends) =
        mixer_with_config(DeckMixerConfig::builder().eq_bands(0).build(), 64);
    start_dc(&mut mixer, &mut ends, &[A]);
    send(
        &mut ends,
        at(BLOCK),
        vec![
            DeckPart::Mix(DeckMixSettingsChange::Volume(
                kithara_signal::FaderValue::checked(first).expect("volume"),
            )),
            DeckPart::Mix(DeckMixSettingsChange::Crossfader(
                CrossfaderGain::checked(0.5).expect("crossfader"),
            )),
        ],
    );
    let manual = render_frames(&mut mixer, BLOCK, 8);
    assert!(
        manual.last().is_some_and(|gain| *gain > 0.25),
        "both manual ramps are still in flight"
    );
    let command = part(
        &mut ends,
        program(
            DeckParameter::Volume,
            BLOCK + 8,
            BLOCK + 24,
            f64::from(first),
            f64::from(first),
        ),
    );
    send(&mut ends, at(BLOCK + 8), vec![command]);
    let entered = render_frames(&mut mixer, BLOCK + 8, 1);
    let expected = 0.5 * (first * first);
    assert_eq!(
        entered[0].to_bits(),
        expected.to_bits(),
        "entry applies the declared factor and the remaining manual target"
    );
    assert!(entered[0].is_finite());
}

#[kithara::test]
fn held_output_factor_stays_outside_later_manual_remainder_ramps() {
    use crate::bridge::{CrossfaderGain, DeckMixSettings, DeckMixSettingsChange};
    let mix = DeckMixSettings::builder()
        .crossfader(CrossfaderGain::checked(0.8).expect("manual crossfader"))
        .build();
    let (mut mixer, mut ends) =
        mixer_with_config(DeckMixerConfig::builder().eq_bands(0).mix(mix).build(), 64);
    start_dc(&mut mixer, &mut ends, &[A]);
    let held = f64::from(0.7_f32);
    let command = part(
        &mut ends,
        program(DeckParameter::Volume, BLOCK, BLOCK + 2, held, held),
    );
    let seq = send(&mut ends, at(BLOCK), vec![command]);
    render_frames(&mut mixer, BLOCK, 4);
    assert_eq!(
        ends.deck
            .snapshot
            .read()
            .parameter(DeckParameter::Volume)
            .expect("volume")
            .program(),
        Some(seq)
    );
    send(
        &mut ends,
        at(BLOCK + 4),
        vec![DeckPart::Mix(DeckMixSettingsChange::Crossfader(
            CrossfaderGain::checked(0.3).expect("new manual crossfader"),
        ))],
    );
    let mut remainder = kithara_dsp::param::SmoothedParam::new(
        0.8,
        crate::consts::GAIN_SPAN,
        SmootherConfig::default(),
        shape().sample_rate,
    );
    remainder.set_value(0.3);
    let expected = [(); 16].map(|()| remainder.next_smoothed() * (0.7_f32 * 0.7));
    let actual = render_frames(&mut mixer, BLOCK + 4, 16);
    assert_eq!(
        actual.into_iter().map(f32::to_bits).collect::<Vec<_>>(),
        expected.map(f32::to_bits)
    );
}
