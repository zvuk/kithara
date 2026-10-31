use kithara_audio::{AudioSource, Fetch, TrackStep};
use kithara_effects::{
    AudioEffect, EffectChange, EffectConfig, EffectParameter, EffectTarget, GainDb,
    eq::{EqBandConfig, EqBandConfigChange, EqConfig, EqEffect},
};
use kithara_render::bridge::{EffectProgram, ParameterKnot};

use super::{
    effects::{
        FRAMES, configured_track, configured_track_with, confirmed_gain, effect_source,
        effect_source_with, input,
    },
    *,
};
use crate::CurveShape;

type CurveSource = kithara_render::WarpSource<kithara_render::mock::ScriptedSource, TestPools>;

pub(super) fn produce(source: &mut CurveSource) -> TrackStep<kithara_signal::AudioChunk> {
    let _observer = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    source
        .service_commands()
        .expect("canonical producer command service");
    let _ = source.prepare_deferred();
    source.finish_deferred();
    assert_no_alloc::assert_no_alloc(|| source.step_track())
}

pub(super) fn playing(track: &mut Track, origin: i64) {
    track.status = TrackStatus::Playing {
        since: frame(origin),
    };
    track.mark = Some(SlotMark {
        session: frame(origin),
        lane: LaneFrame::default(),
        position: Duration::ZERO,
    });
}

pub(super) fn render(
    track: &mut Track,
    rig: &mut Rig,
    source: &mut CurveSource,
) -> (Vec<f32>, Vec<(u64, usize)>) {
    let _observer = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    let mut pcm = Vec::new();
    let mut spans = Vec::new();
    let mut ended = false;
    let remaining = usize::try_from(u64::from(FRAMES) - source.cursor().frame).unwrap();
    for _ in 0..FRAMES * 4 {
        let step = produce(source);
        match step {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                spans.push((data.meta.lane_frame, data.frames()));
                pcm.extend_from_slice(&data.samples);
                source.admitted();
                in_pass(rig, frame(0), |out| track.tick(frame(0), out));
            }
            TrackStep::StateChanged => {}
            TrackStep::Eof => {
                ended = true;
                break;
            }
            _ => panic!("an admitted curve must preserve finite source PCM"),
        }
    }
    assert!(ended);
    assert_eq!(pcm.len(), remaining * 2);
    (pcm, spans)
}

pub(super) fn linear_program(
    origin: i64,
    effect: usize,
    start: i64,
    end: i64,
    from: f64,
    to: f64,
) -> EffectProgram {
    EffectProgram::new(
        EffectParameter::EqGain { effect, band: 0 },
        vec![
            ParameterKnot::new(frame(origin + start), from, CurveShape::LINEAR).unwrap(),
            ParameterKnot::new(frame(origin + end), to, CurveShape::HOLD).unwrap(),
        ],
    )
    .unwrap()
}

#[kithara::test]
async fn effect_curves_apply_every_frame_without_manual_smoothing() {
    use kithara_effects::EffectParameter;
    use kithara_render::bridge::{EffectProgram, ParameterKnot};

    use crate::CurveShape;

    let region = pools();
    let mut rig = rig();
    let (mut track, inbox) = configured_track(&mut rig).await;
    let lane = track.lane_id.unwrap();
    let anchor = (1_i64 << 40) + 4096;
    track.status = TrackStatus::Playing {
        since: frame(anchor),
    };
    track.mark = Some(SlotMark {
        session: frame(anchor),
        lane: LaneFrame::default(),
        position: Duration::ZERO,
    });
    let program = EffectProgram::new(
        EffectParameter::EqGain { effect: 0, band: 0 },
        [
            (17, 0.0, CurveShape::cubic(0.0, 1.0).unwrap()),
            (33, -12.0, CurveShape::HOLD),
            (40, -12.0, CurveShape::LINEAR),
            (48, 6.0, CurveShape::HOLD),
        ]
        .into_iter()
        .map(|(at, value, shape)| ParameterKnot::new(frame(anchor + at), value, shape).unwrap())
        .collect(),
    )
    .unwrap();
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::EffectProgram { lane, program },
    )
    .unwrap();
    assert_eq!(confirmed_gain(&track), GainDb::DEFAULT);
    let mut source = effect_source(inbox, &region);
    let mut pcm = Vec::new();
    for _ in 0..64 {
        let _ = source.prepare_deferred();
        source.finish_deferred();
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                source.admitted();
                pcm.extend_from_slice(&data.samples);
                in_pass(&mut rig, frame(0), |out| track.tick(frame(0), out));
            }
            TrackStep::StateChanged => {}
            TrackStep::Eof => break,
            _ => panic!("prepared curve did not produce finite PCM"),
        }
    }
    assert_eq!(pcm.len(), FRAMES as usize * 2);
    for (at, samples) in pcm.as_chunks::<2>().0.iter().enumerate() {
        let db = match at {
            0..17 => 0.0,
            17..33 => {
                let t = f64::from(u32::try_from(at - 17).unwrap()) / 16.0;
                -12.0 * (3.0 * t * t - 2.0 * t * t * t)
            }
            33..40 => -12.0,
            40..48 => -12.0 + 18.0 * f64::from(u32::try_from(at - 40).unwrap()) / 8.0,
            _ => 6.0,
        };
        let db: f32 = num_traits::cast(db).unwrap();
        let expected = 0.25 * 10.0_f32.powf(db / 20.0);
        assert_eq!(*samples, [expected, expected], "frame {at}");
    }
    assert_eq!(confirmed_gain(&track), GainDb::MAX);
}

#[kithara::test]
async fn effect_curve_subdivision_does_not_insert_control_rate_packets() {
    let region = pools();
    let mut reference = None;
    for quantum in [1, 7, 13, 128] {
        let mut rig = rig();
        let (mut track, inbox) = configured_track(&mut rig).await;
        let origin = (1_i64 << 40) + 4096;
        playing(&mut track, origin);
        let lane = track.lane_id.unwrap();
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::EffectProgram {
                lane,
                program: linear_program(origin, 0, 7, 87, 0.0, -12.0),
            },
        )
        .unwrap();
        let (pcm, spans) = render(
            &mut track,
            &mut rig,
            &mut effect_source_with(inbox, &region, quantum, 1),
        );
        if quantum == 128 {
            assert_eq!(spans, [(0, 7), (7, 80), (87, 9)]);
        }
        for (at, channels) in pcm.as_chunks::<2>().0.iter().enumerate() {
            let db = if at < 7 {
                0.0
            } else if at >= 87 {
                -12.0
            } else {
                -12.0 * (f64::from(u32::try_from(at - 7).unwrap()) / 80.0)
            };
            let db: f32 = num_traits::cast(db).unwrap();
            let expected = 0.25 * 10.0_f32.powf(db / 20.0);
            assert_eq!(*channels, [expected; 2], "quantum {quantum}, frame {at}");
        }
        if let Some(reference) = &reference {
            assert_eq!(&pcm, reference);
        } else {
            reference = Some(pcm);
        }
        assert_eq!(confirmed_gain(&track), GainDb::from(-12.0));
    }
}

#[kithara::test]
async fn manual_step_after_a_curve_keeps_another_field_running() {
    let region = pools();
    for quantum in [1, 13, 128] {
        let mut rig = rig();
        let (mut track, inbox) = configured_track_with(
            &mut rig,
            vec![EffectConfig::Equalizer(vec![EqBandConfig::default()]); 2],
        )
        .await;
        let origin = (1_i64 << 40) + 4096;
        playing(&mut track, origin);
        let lane = track.lane_id.unwrap();
        for program in [
            linear_program(origin, 0, 17, 79, 0.0, -12.0),
            linear_program(origin, 1, 21, 45, -6.0, 6.0),
        ] {
            command_at_zero(
                &mut track,
                &mut rig,
                TrackCommand::EffectProgram { lane, program },
            )
            .unwrap();
        }
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::ConfigureEffect {
                lane,
                target: EffectTarget::EqBand { effect: 1, band: 0 },
                change: EffectChange::EqBand(EqBandConfigChange::GainDb(GainDb::MIN)),
                at: When::At(frame(origin + 53)),
            },
        )
        .unwrap();
        let (pcm, _) = render(
            &mut track,
            &mut rig,
            &mut effect_source_with(inbox, &region, quantum, 2),
        );
        let mut manual = EqEffect::new(
            &EqConfig::builder(region.clone()).build(),
            vec![EqBandConfig::builder().gain_db(GainDb::MAX).build()],
            mock::SAMPLE_RATE.get(),
            2,
        )
        .unwrap();
        manual.set_gain(0, GainDb::MIN);
        let mut suffix = input(&region, FRAMES - 53);
        for (at, channels) in pcm.as_chunks::<2>().0.iter().enumerate() {
            let db = if at < 17 {
                0.0
            } else if at >= 79 {
                -12.0
            } else {
                -12.0 * (f64::from(u32::try_from(at - 17).unwrap()) / 62.0)
            };
            let db: f32 = num_traits::cast(db).unwrap();
            let first = 0.25 * 10.0_f32.powf(db / 20.0);
            if at >= 53 {
                suffix.samples[(at - 53) * 2..(at - 53) * 2 + 2].fill(first);
            } else {
                let db = if at < 21 {
                    0.0
                } else if at >= 45 {
                    6.0
                } else {
                    -6.0 + 12.0 * (f64::from(u32::try_from(at - 21).unwrap()) / 24.0)
                };
                let db: f32 = num_traits::cast(db).unwrap();
                let expected = first * 10.0_f32.powf(db / 20.0);
                assert_eq!(*channels, [expected; 2], "frame {at}");
            }
        }
        let expected = manual.process(suffix).unwrap();
        assert_eq!(&pcm[53 * 2..], &expected.samples[..]);
        let snapshot = track.snapshot();
        let EffectSnapshot::Equalizer { bands, .. } = &snapshot.effects[1];
        assert_eq!(bands[0].config().gain_db(), GainDb::MIN);
        assert_eq!(confirmed_gain(&track), GainDb::from(-12.0));
    }
}
