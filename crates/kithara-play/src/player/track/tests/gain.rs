use kithara_command::Target;
use kithara_render::{
    bridge::{ParameterKnot, SlotGainProgram},
    mock::pcm_samples,
    rt::StreamShape,
};

use super::*;
use crate::CurveShape;

// Allocation cases exclude the test flight recorder, which allocates per probe.
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const ORIGIN: i64 = (1 << 25) + 128;

fn knot(at: i64, value: f64, shape: CurveShape) -> ParameterKnot {
    ParameterKnot::new(frame(at), value, shape).expect("finite fixture knot")
}

fn settle_outputs(
    tracks: &mut [Track],
    mixer: &mut mock::MixerRig,
    dispatcher: &mut Sender<DispatcherProtocol<ResourceLoad<TestPools>>>,
    at: SessionFrame,
) {
    while let Some(reply) = mixer.ring.receipt() {
        let kithara_command::ScopedReceipt::Scope(_, receipt) = reply else {
            panic!("track programs use the existing deck scope");
        };
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        for track in &mut *tracks {
            with_mixer(mixer, dispatcher, at, |out| {
                track.settle(
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            });
        }
    }
}

struct PreparedOutputs {
    mixer: mock::MixerRig,
    dispatcher: Sender<DispatcherProtocol<ResourceLoad<TestPools>>>,
    tracks: [Track; 2],
    _lanes: [Inbox<LaneProtocol>; 2],
}

fn prepared_outputs() -> PreparedOutputs {
    let mut declick = DeckMixerConfig::default().declick();
    declick.smooth_seconds = 0.0;
    let config = DeckMixerConfig::builder()
        .eq_bands(0)
        .declick(declick)
        .build();
    let shape = StreamShape::new(NonZeroU32::new(64).expect("block"), mock::SAMPLE_RATE);
    let mut mixer = mock::MixerRig::new(config, shape, &pools()).expect("real processor");
    let (mut dispatcher, _dispatch_inbox) = channel(ChannelConfig::builder().build());
    let (first, first_lane) = prepared_track();
    let (mut second, second_lane) = prepared_track();
    second.slot = Some(B);
    let mut tracks = [first, second];
    for ((track, slot), sample) in tracks.iter().zip([A, B]).zip([0.5, -0.25]) {
        mixer
            .send(
                When::At(frame(ORIGIN)),
                DeckPart::Attach {
                    slot,
                    item_id: track.item,
                    pcm: pcm_samples("prepared combined PCM", 1.0, &[sample; 512], true),
                    segment: SegmentId::FIRST,
                },
            )
            .expect("prepared source attachment");
    }
    mixer
        .block_frames(frame(ORIGIN), [&mut [0.0; 1], &mut [0.0; 1]])
        .expect("attachment frame");
    settle_outputs(&mut tracks, &mut mixer, &mut dispatcher, frame(ORIGIN));
    for track in &mut tracks {
        with_mixer(&mut mixer, &mut dispatcher, frame(ORIGIN + 1), |out| {
            track.apply(TrackCommand::Play { at: When::Next }, out)
        })
        .expect("prepared track starts");
    }
    let (mut left, mut right) = ([0.0; 1], [0.0; 1]);
    mixer
        .block_frames(frame(ORIGIN + 1), [&mut left, &mut right])
        .expect("first prepared PCM");
    settle_outputs(&mut tracks, &mut mixer, &mut dispatcher, frame(ORIGIN + 1));
    assert_eq!(left, [0.25]);
    assert_eq!(right, left);
    assert!(
        tracks
            .iter()
            .all(|track| matches!(track.status(), TrackStatus::Playing { .. }))
    );
    PreparedOutputs {
        mixer,
        dispatcher,
        tracks,
        _lanes: [first_lane, second_lane],
    }
}

fn programs() -> [SlotGainProgram; 2] {
    [
        SlotGainProgram::new(vec![
            knot(
                ORIGIN + 5,
                1.0,
                CurveShape::cubic(0.0, 1.0).expect("S shape"),
            ),
            knot(ORIGIN + 21, 0.0, CurveShape::HOLD),
        ])
        .expect("descending cubic"),
        SlotGainProgram::new(vec![
            knot(ORIGIN + 9, 0.0, CurveShape::LINEAR),
            knot(ORIGIN + 17, 0.5, CurveShape::HOLD),
            knot(ORIGIN + 25, 0.25, CurveShape::HOLD),
        ])
        .expect("independent linear and held gains"),
    ]
}

fn expected(frame: u32) -> f32 {
    let first = if frame < 5 {
        1.0
    } else if frame >= 21 {
        0.0
    } else {
        let time = f64::from(frame - 5) / 16.0;
        1.0 - (3.0 * time * time - 2.0 * time * time * time)
    };
    let second = match frame {
        0..9 => 1.0,
        9..17 => f64::from(frame - 9) / 16.0,
        17..25 => 0.5,
        _ => 0.25,
    };
    let first = first.to_f32().expect("bounded gain");
    let second = second.to_f32().expect("bounded gain");
    0.5 * first - 0.25 * second
}

#[kithara::test]
fn slot_gain_envelopes_are_independent_during_overlap() {
    for block in [1, 3, 7, 32] {
        let PreparedOutputs {
            mut mixer,
            mut dispatcher,
            mut tracks,
            _lanes,
        } = prepared_outputs();
        let mut sequences = Vec::new();
        for (track, program) in tracks.iter_mut().zip(programs()) {
            let seq = with_mixer(&mut mixer, &mut dispatcher, frame(ORIGIN + 2), |out| {
                track.apply(TrackCommand::GainProgram(program), out)
            })
            .expect("program admitted")
            .expect("program owns an ordinary deck command");
            sequences.push(seq);
        }
        for slot in &mixer.ends.snapshot.read().slots[..2] {
            assert_eq!(slot.output.gain(), 1.0);
            assert_eq!(
                slot.output.program(),
                None,
                "future intent is not sounding state"
            );
        }
        let mut pcm = Vec::new();
        for offset in (0..32).step_by(block) {
            let length = block.min(32 - offset);
            let (mut left, mut right) = (vec![0.0; length], vec![0.0; length]);
            mixer
                .block_frames(
                    frame(ORIGIN + 2 + i64::try_from(offset).expect("small offset")),
                    [&mut left, &mut right],
                )
                .expect("real processor consumes programs");
            assert_eq!(left, right);
            pcm.extend(left);
        }
        let oracle: Vec<_> = (2..34).map(expected).collect();
        assert_eq!(pcm, oracle);
        assert_ne!(
            pcm,
            vec![0.25; 32],
            "the oracle rejects unchanged complete output"
        );
        let slots = &mixer.ends.snapshot.read().slots;
        assert_eq!(slots[0].output.gain(), 0.0);
        assert_eq!(slots[1].output.gain(), 0.25);
        assert_eq!(slots[0].output.program(), Some(sequences[0]));
        assert_eq!(slots[1].output.program(), Some(sequences[1]));
        assert_eq!(mixer.ends.snapshot.read().metrics.underruns(), 0);
    }
}

#[kithara::test]
fn slot_gain_automation_is_bounded_and_rt_safe() {
    let _observer = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    let PreparedOutputs {
        mut mixer,
        mut dispatcher,
        mut tracks,
        _lanes,
    } = prepared_outputs();
    let program =
        SlotGainProgram::new(vec![knot(ORIGIN + 5, 0.0, CurveShape::HOLD)]).expect("exact mute");
    with_mixer(&mut mixer, &mut dispatcher, frame(ORIGIN + 2), |out| {
        tracks[0].apply(TrackCommand::GainProgram(program), out)
    })
    .expect("prepared output control");
    let (mut left, mut right) = ([0.0; 32], [0.0; 32]);
    assert_no_alloc::assert_no_alloc(|| {
        mixer
            .block_frames(frame(ORIGIN + 2), [&mut left, &mut right])
            .expect("bounded real processing");
    });
    assert_eq!(&left[..3], &[0.25; 3]);
    assert_eq!(&left[3..], &[-0.25; 29]);
    assert_eq!(right, left);
    assert_eq!(mixer.ends.snapshot.read().metrics.underruns(), 0);
    for value in [f64::NAN, f64::INFINITY] {
        assert!(ParameterKnot::new(frame(ORIGIN + 40), value, CurveShape::HOLD).is_err());
    }
    assert!(SlotGainProgram::new(Vec::new()).is_err());
    assert!(
        SlotGainProgram::new(vec![
            knot(ORIGIN + 40, 1.0, CurveShape::LINEAR),
            knot(ORIGIN + 40, 0.0, CurveShape::HOLD),
        ])
        .is_err()
    );
    assert!(SlotGainProgram::new(vec![knot(ORIGIN + 40, -0.1, CurveShape::HOLD)]).is_err());
    assert!(
        SlotGainProgram::new(
            (0..=64)
                .map(|offset| { knot(ORIGIN + 40 + offset, 0.5, CurveShape::HOLD) })
                .collect()
        )
        .is_err()
    );
}

#[kithara::test]
fn gain_program_late_and_unready_requests_leave_applied_pcm_unchanged() {
    let PreparedOutputs {
        mut mixer,
        mut dispatcher,
        mut tracks,
        _lanes,
    } = prepared_outputs();
    let late = SlotGainProgram::new(vec![knot(ORIGIN + 1, 0.0, CurveShape::HOLD)])
        .expect("finite exact step");
    let result = with_mixer(&mut mixer, &mut dispatcher, frame(ORIGIN + 2), |out| {
        tracks[0].apply(TrackCommand::GainProgram(late), out)
    });
    assert!(matches!(result, Err(PlayError::Late)));
    tracks[0].ready = None;
    let result = with_mixer(&mut mixer, &mut dispatcher, frame(ORIGIN + 2), |out| {
        tracks[0].apply(TrackCommand::GainProgram(programs()[0].clone()), out)
    });
    assert!(matches!(result, Err(PlayError::NotReady)));
    let (mut left, mut right) = ([0.0; 32], [0.0; 32]);
    mixer
        .block_frames(frame(ORIGIN + 2), [&mut left, &mut right])
        .expect("original PCM");
    assert_eq!(left, [0.25; 32]);
    assert_eq!(right, left);
    assert_eq!(mixer.ends.snapshot.read().slots[0].output.gain(), 1.0);
    assert_eq!(mixer.ends.snapshot.read().slots[0].output.program(), None);
    assert!(
        mixer.ring.receipt().is_none(),
        "refused intent never entered the deck"
    );
}

#[kithara::test]
fn future_gain_program_cannot_mute_a_reused_attachment() {
    let _observer = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    let PreparedOutputs {
        mut mixer,
        dispatcher: _dispatcher,
        tracks,
        _lanes,
    } = prepared_outputs();
    let observed = mixer.ends.snapshot.read().slots[0];
    let binding = observed.binding.expect("original sounding attachment");
    let program =
        SlotGainProgram::new(vec![knot(ORIGIN + 5, 0.0, CurveShape::HOLD)]).expect("finite mute");
    let stale = mixer
        .send(
            When::At(frame(ORIGIN + 5)),
            DeckPart::GainProgram {
                slot: A,
                binding,
                authority: observed.output.authority(),
                program,
            },
        )
        .expect("future original attachment program");
    let replacement = mixer
        .send(
            When::At(frame(ORIGIN + 4)),
            DeckPart::Replace {
                slot: A,
                item_id: tracks[0].item,
                pcm: pcm_samples("replacement PCM", 1.0, &[0.125; 512], true),
                segment: SegmentId::FIRST,
            },
        )
        .expect("replacement in the same slot and segment");
    let (mut left, mut right) = ([0.0; 8], [0.0; 8]);
    assert_no_alloc::assert_no_alloc(|| {
        mixer
            .block_frames(frame(ORIGIN + 2), [&mut left, &mut right])
            .expect("replacement processing");
    });
    assert_eq!(&left[..2], &[0.25; 2]);
    // The old consumer retains the declared 512-frame eviction ramp.
    for (offset, sample) in left[2..].iter().enumerate() {
        let progress = offset.to_f32().expect("small tail offset") / 511.0;
        assert_eq!(*sample, -0.125 + 0.5 * (1.0 - progress));
    }
    assert_eq!(right, left);
    let mut received = Vec::new();
    while let Some(reply) = mixer.ring.receipt() {
        let kithara_command::ScopedReceipt::Scope(_, receipt) = reply else {
            panic!("deck receipt")
        };
        let seq = receipt.seq();
        let (outcome, _batch) = receipt.into();
        received.push((seq, outcome));
    }
    assert_eq!(received.len(), 2);
    assert!(received.iter().any(|(seq, outcome)| *seq == replacement && matches!(outcome, Outcome::Applied { .. })));
    assert!(received.iter().any(|(seq, outcome)| *seq == stale
        && matches!(outcome, Outcome::Rejected(Rejection::Refused(DeckRefusal::Outdated { slot })) if *slot == A)));
    assert_eq!(mixer.ends.snapshot.read().slots[0].output.gain(), 1.0);
    assert_eq!(mixer.ends.snapshot.read().slots[0].output.program(), None);
    assert_eq!(mixer.ends.snapshot.read().metrics.underruns(), 0);
}

#[kithara::test]
fn output_cubic_composes_with_existing_equal_power_track_fade() {
    for block in [1, 3, 7, 32] {
        let PreparedOutputs {
            mut mixer,
            mut dispatcher,
            mut tracks,
            _lanes,
        } = prepared_outputs();
        with_mixer(&mut mixer, &mut dispatcher, frame(ORIGIN + 2), |out| {
            tracks[0].apply(TrackCommand::GainProgram(programs()[0].clone()), out)
        })
        .expect("prepared cubic");
        let fade = CrossfadeSettings {
            duration: 17.0 / mock::SAMPLE_RATE.get().to_f32().expect("rate fits f32"),
            ..CrossfadeSettings::default()
        };
        mixer
            .send(
                When::At(frame(ORIGIN + 5)),
                DeckPart::Fade {
                    slot: A,
                    settings: fade,
                    dir: FadeDir::Out,
                },
            )
            .expect("ordinary equal-power TrackFade");
        let mut pcm = Vec::new();
        for offset in (0..32).step_by(block) {
            let length = block.min(32 - offset);
            let (mut left, mut right) = (vec![0.0; length], vec![0.0; length]);
            mixer
                .block_frames(
                    frame(ORIGIN + 2 + i64::try_from(offset).expect("small offset")),
                    [&mut left, &mut right],
                )
                .expect("composed output");
            assert_eq!(left, right);
            pcm.extend(left);
        }
        let oracle: Vec<_> = (2..34_u32)
            .map(|at| {
                let gain = if at < 5 {
                    1.0
                } else if at >= 21 {
                    0.0
                } else {
                    let t = f64::from(at - 5) / 16.0;
                    (1.0 - (3.0 * t * t - 2.0 * t * t * t))
                        .to_f32()
                        .expect("bounded cubic")
                };
                let outgoing = if at < 5 {
                    1.0
                } else {
                    fade.gains((at - 5).to_f32().expect("small frame") / 16.0).0
                };
                (0.5 * gain) * outgoing - 0.25
            })
            .collect();
        assert_eq!(pcm, oracle);
        assert_eq!(&pcm[19..], &[-0.25; 13]);
        assert_eq!(mixer.ends.snapshot.read().slots[0].output.gain(), 0.0);
        assert_eq!(mixer.ends.snapshot.read().metrics.underruns(), 0);
    }
}

#[kithara::test]
fn committed_gain_batch_returns_only_after_its_last_slot_finishes() {
    let _observer = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    let PreparedOutputs {
        mut mixer,
        dispatcher: _dispatcher,
        tracks: _tracks,
        _lanes,
    } = prepared_outputs();
    let parts = [A, B]
        .into_iter()
        .zip([9, 13])
        .map(|(slot, end)| {
            let observed = mixer.ends.snapshot.read().slots[slot.index()];
            let program = SlotGainProgram::new(vec![
                knot(ORIGIN + 5, 1.0, CurveShape::LINEAR),
                knot(ORIGIN + end, 0.5, CurveShape::HOLD),
            ])
            .expect("bounded shared batch");
            DeckPart::GainProgram {
                slot,
                binding: observed.binding.expect("prepared attachment"),
                authority: observed.output.authority(),
                program,
            }
        })
        .collect();
    let seq = mixer
        .ring
        .scope(mixer.scope)
        .expect("deck scope")
        .send(
            When::At(frame(ORIGIN + 5)),
            Batch {
                commands: parts,
                basis: Vec::new(),
            },
        )
        .expect("one committed batch for two parameters");
    let (mut left, mut right) = ([0.0; 9], [0.0; 9]);
    assert_no_alloc::assert_no_alloc(|| {
        mixer
            .block_frames(frame(ORIGIN + 2), [&mut left, &mut right])
            .expect("first program endpoint");
    });
    assert!(
        mixer.ring.receipt().is_none(),
        "the other slot still owns its knots"
    );
    assert_eq!(mixer.ends.snapshot.read().slots[0].output.gain(), 0.5);
    assert_eq!(mixer.ends.snapshot.read().slots[1].output.gain(), 0.6875);
    let (mut left, mut right) = ([0.0; 4], [0.0; 4]);
    assert_no_alloc::assert_no_alloc(|| {
        mixer
            .block_frames(frame(ORIGIN + 11), [&mut left, &mut right])
            .expect("last program endpoint");
    });
    assert_eq!(left, [0.09375, 0.109375, 0.125, 0.125]);
    assert_eq!(right, left);
    let Some(kithara_command::ScopedReceipt::Scope(_, receipt)) = mixer.ring.receipt() else {
        panic!("completed programs return to the control owner");
    };
    assert_eq!(receipt.seq(), seq);
    let (outcome, batch) = receipt.into();
    assert!(matches!(outcome, Outcome::Applied { at, .. } if at == frame(ORIGIN + 5)));
    assert_eq!(batch.commands.len(), 2);
    assert!(
        batch
            .commands
            .iter()
            .all(|part| matches!(part, DeckPart::GainProgram { .. }))
    );
    assert!(mixer.ring.receipt().is_none());
    for slot in &mixer.ends.snapshot.read().slots[..2] {
        assert_eq!(slot.output.gain(), 0.5);
        assert_eq!(slot.output.program(), Some(seq));
    }
}

#[kithara::test]
fn muted_complete_output_stays_muted_in_the_evicted_consumer_tail() {
    let _observer = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    let PreparedOutputs {
        mut mixer,
        mut dispatcher,
        mut tracks,
        _lanes,
    } = prepared_outputs();
    let program = SlotGainProgram::new(vec![knot(ORIGIN + 5, 0.0, CurveShape::HOLD)])
        .expect("exact complete-output mute");
    with_mixer(&mut mixer, &mut dispatcher, frame(ORIGIN + 2), |out| {
        tracks[0].apply(TrackCommand::GainProgram(program), out)
    })
    .expect("prepared mute");
    mixer
        .send(
            When::At(frame(ORIGIN + 7)),
            DeckPart::Replace {
                slot: A,
                item_id: tracks[0].item,
                pcm: pcm_samples("unmuted replacement", 1.0, &[0.125; 512], true),
                segment: SegmentId::FIRST,
            },
        )
        .expect("new source after the old complete output was muted");
    let (mut left, mut right) = ([0.0; 16], [0.0; 16]);
    assert_no_alloc::assert_no_alloc(|| {
        mixer
            .block_frames(frame(ORIGIN + 2), [&mut left, &mut right])
            .expect("mute and replacement with retained old PCM");
    });
    assert_eq!(&left[..3], &[0.25; 3]);
    assert_eq!(&left[3..5], &[-0.25; 2]);
    assert_eq!(&left[5..], &[-0.125; 11]);
    assert_eq!(right, left);
    assert_eq!(mixer.ends.snapshot.read().metrics.underruns(), 0);
}
