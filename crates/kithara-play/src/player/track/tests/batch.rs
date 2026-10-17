use std::num::NonZeroUsize;

use kithara_audio::TrackFailureKind;
use kithara_command::Target;

use super::*;
use crate::Bound;

#[kithara::test]
#[case::ready(true)]
#[case::rejected(false)]
fn a_segment_entry_keeps_its_native_curve_and_waits_for_native_readiness(#[case] applied: bool) {
    let mut rig = rig();
    let (mut track, mut inbox) = prepared_track();
    let curve = SpeedCurve::Approach(
        kithara_signal::ExponentialApproach::new(1.0, 0.9, 220.5).expect("finite curve"),
    );
    let position = Duration::from_secs(2);
    in_pass(&mut rig, frame(0), |out| {
        track.cue(position, curve.clone(), out)
    })
    .expect("cue");
    let segment = track.segment;
    let bound = Bound::AtOrAfter(frame(128));
    assert_eq!(track.ready, None);
    assert_eq!(track.entry(bound), None);
    track.status = TrackStatus::Loading;
    assert_eq!(track.entry(bound), None);
    assert_ne!(
        track.lane_commands.last().expect("segment command").segment,
        segment
    );
    assert_eq!(track.prepared_speed(), Some(&curve));
    assert!(
        !in_pass(&mut rig, frame(0), |out| track
            .prepare_entry(frame(128), out))
        .expect("preparation")
    );
    inbox.drain();
    let due = inbox
        .next_due(LaneFrame { segment, frame: 0 }, 1)
        .expect("native segment");
    assert!(
        matches!(due.commands(), [LaneCommand::Segment { id, speed, from }] if *id == segment && *speed == curve && *from == position)
    );
    if applied {
        due.apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: Some(segment),
        });
    } else {
        drop(due);
    }
    assert_eq!(
        in_pass(&mut rig, frame(0), |out| track
            .prepare_entry(frame(128), out))
        .expect("native receipt"),
        applied
    );
    assert_eq!(track.prepared_speed(), applied.then_some(&curve));
    assert_eq!(track.ready, applied.then_some(segment));
    assert_eq!(track.entry(bound), applied.then_some(frame(128)));
    track.status = TrackStatus::Loaded;
    assert_eq!(track.entry(bound), applied.then_some(frame(128)));
    if !applied {
        for host_rate in [false, true] {
            for status in [
                TrackStatus::Loaded,
                TrackStatus::Paused { at: Duration::ZERO },
                TrackStatus::Loading,
            ] {
                assert_preparation_refusal(host_rate, status, false);
            }
            assert_preparation_refusal(host_rate, TrackStatus::Loaded, true);
            assert_obsolete_preparation_refusal(host_rate);
            for tick_before_receipt in [false, true] {
                for release_before_receipt in [false, true] {
                    assert_failed_replacement_keeps_incumbent(
                        host_rate,
                        tick_before_receipt,
                        release_before_receipt,
                    );
                }
            }
        }
        assert_terminal_adoption_refusal(false, false);
        assert_terminal_adoption_refusal(false, true);
        assert_terminal_adoption_refusal(true, false);
    }
}

fn pause_prepared_track(track: &mut Track, rig: &mut Rig) {
    let status = track.snapshot().status;
    let position = track.snapshot().position;
    let resume = track.resume;
    let pause = command_at_zero(track, rig, TrackCommand::Pause { at: When::Next })
        .expect("native Pause sequence");
    assert_eq!(track.snapshot().status, status);
    assert_eq!(track.snapshot().position, position);
    assert_eq!(track.resume, resume);
    let stopped_mark = SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: track.segment,
            frame: 0,
        },
        position: Duration::ZERO,
    };
    let mut stopped = rig.block(frame(0), 0.0);
    assert!(matches!(
        stopped.as_slice(),
        [answer] if answer.seq == pause
            && matches!(answer.outcome, Outcome::Applied { at, .. } if at == frame(0))
            && matches!(answer.batch.commands.as_slice(),
                [DeckPart::Returned(Returned::Stopped { slot, resume: Some(mark) })]
                    if *slot == A && *mark == stopped_mark)
    ));
    assert_eq!(track.snapshot().status, status);
    assert_eq!(track.snapshot().position, position);
    assert_eq!(track.resume, resume);
    let settled = settle(track, rig, &mut stopped);
    assert!(matches!(
        settled.as_slice(),
        [Settled::Applied { seq, at }] if *seq == pause && *at == frame(0)
    ));
    assert_eq!(track.resume, Some(stopped_mark));
}

fn assert_preparation_refusal(host_rate: bool, initial: TrackStatus, reserved: bool) {
    let mut rig = rig();
    let (mut track, mut inbox) = if reserved || initial == TrackStatus::Loading {
        let mut track = track(A);
        if reserved {
            track.slot = None;
        }
        load(&mut track, &mut rig, Duration::ZERO);
        let opened = opened_fixture(&mut rig, "refused-preparation");
        let receipt = rig.open(Ok(opened)).expect("native opened source");
        rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
            .expect("live scope");
        let (sender, inbox) = lane();
        track.lane = Some(sender);
        (track, inbox)
    } else if matches!(initial, TrackStatus::Paused { .. }) {
        loaded(&mut rig, A, "refused-paused-preparation")
    } else {
        prepared_track()
    };
    if matches!(initial, TrackStatus::Paused { .. }) {
        pause_prepared_track(&mut track, &mut rig);
    } else if initial == TrackStatus::Loaded && !reserved {
        command_at_zero(&mut track, &mut rig, TrackCommand::Play { at: When::Next });
    }
    assert_eq!(track.status(), initial);
    command_at_zero(
        &mut track,
        &mut rig,
        if host_rate {
            TrackCommand::SetHostRate {
                rate: mock::SAMPLE_RATE,
            }
        } else {
            TrackCommand::Seek { to: Duration::ZERO }
        },
    );
    let segment = track.segment;
    command_at_zero(&mut track, &mut rig, TrackCommand::Play { at: When::Next });
    assert_eq!(track.play, Some(When::Next));
    let prepared = track.lane_commands.last().expect("native preparation").seq;
    let later = track
        .send_lane(LaneCommand::SetSpeed(SpeedCurve::Constant(1.0)), When::Next)
        .expect("later native control");
    assert_ne!(prepared, later);
    inbox.drain();
    let due = inbox
        .next_due(LaneFrame { segment, frame: 0 }, 1)
        .expect("native preparation refusal");
    assert_eq!(due.seq(), prepared);
    let failure = TrackFailureKind::Render;
    due.refuse(failure);
    assert!(matches!(
        in_pass(&mut rig, frame(32), |out| track.prepare_entry(frame(128), out)),
        Err(PlayError::Lane(reason)) if reason == failure
    ));
    assert_eq!(track.entry(Bound::AtOrAfter(frame(128))), None);
    assert!(matches!(
        in_pass(&mut rig, frame(32), |out| track.admit(
            TrackSettingsChange::Speed(1.25), When::Next, out
        )),
        Err(PlayError::Lane(reason)) if reason == failure
    ));
    in_pass(&mut rig, frame(64), |out| track.tick(frame(64), out));
    let failed = TrackStatus::Failed {
        at: frame(64),
        fault: PlaybackFault::Source(failure),
    };
    assert_eq!(track.status(), failed);
    assert_eq!(track.snapshot().status, failed);
    assert_eq!(track.play, None);
    assert_eq!(track.ready, None);
    if let Some(attaching) = track.attaching {
        assert_eq!(attaching.play, None);
    }
    let receipts = track.adopting.len()
        + track.playback_commands.len()
        + usize::from(track.attaching.is_some());
    for _ in 0..receipts {
        let receipt = answer_next(&mut rig, Ok(()));
        answer_track(&mut track, &mut rig, receipt);
        assert_eq!(track.snapshot().status, failed);
    }
    inbox
        .next_due(LaneFrame { segment, frame: 0 }, 1)
        .expect("later control refusal")
        .refuse(TrackFailureKind::ChannelClosed);
    in_pass(&mut rig, frame(256), |out| track.tick(frame(256), out));
    assert_eq!(track.snapshot().status, failed);
    assert_eq!(track.play, None);
    assert_eq!(track.entry(Bound::AtOrAfter(frame(384))), None);
    assert!(!track.snapshot().pending_lane);
}

fn assert_obsolete_preparation_refusal(host_rate: bool) {
    let mut rig = rig();
    let (mut track, mut inbox) = prepared_track();
    command_at_zero(
        &mut track,
        &mut rig,
        if host_rate {
            TrackCommand::SetHostRate {
                rate: mock::SAMPLE_RATE,
            }
        } else {
            TrackCommand::Seek { to: Duration::ZERO }
        },
    );
    let current = track.segment;
    let obsolete = track.lane_commands.last().expect("first preparation").seq;
    let replacement = track
        .send_lane(
            if host_rate {
                LaneCommand::SetHostRate {
                    id: current,
                    rate: mock::SAMPLE_RATE,
                }
            } else {
                LaneCommand::Segment {
                    id: current,
                    from: Duration::ZERO,
                    speed: SpeedCurve::Constant(1.0),
                }
            },
            When::Next,
        )
        .expect("same-segment replacement");
    assert_ne!(obsolete, replacement);
    assert!(
        track
            .lane_commands
            .iter()
            .all(|operation| matches!(operation.command,
        LaneCommand::Segment { id, .. } | LaneCommand::SetHostRate { id, .. } if id == current))
    );
    inbox.drain();
    let due = inbox
        .next_due(
            LaneFrame {
                segment: current,
                frame: 0,
            },
            1,
        )
        .expect("obsolete preparation");
    assert_eq!(due.seq(), obsolete);
    due.refuse(TrackFailureKind::Render);
    assert!(
        !in_pass(&mut rig, frame(64), |out| track
            .prepare_entry(frame(128), out))
        .expect("obsolete same-segment refusal cannot reject replacement")
    );
    in_pass(&mut rig, frame(64), |out| track.tick(frame(64), out));
    assert_eq!(track.snapshot().status, TrackStatus::Loaded);
    let due = inbox
        .next_due(
            LaneFrame {
                segment: current,
                frame: 0,
            },
            1,
        )
        .expect("replacement preparation");
    assert_eq!(due.seq(), replacement);
    due.refuse(TrackFailureKind::SourceCancelled);
    assert!(matches!(
        in_pass(&mut rig, frame(128), |out| track
            .prepare_entry(frame(256), out)),
        Err(PlayError::Lane(TrackFailureKind::SourceCancelled))
    ));
    in_pass(&mut rig, frame(128), |out| track.tick(frame(128), out));
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Failed {
            at: frame(128),
            fault: PlaybackFault::Source(TrackFailureKind::SourceCancelled),
        }
    );
}

fn playing_incumbent(support: &mut Rig) -> (mock::MixerRig, [[f32; 32]; 2]) {
    let incumbent = opened_fixture(support, "failed-replacement-incumbent").opened;
    let shape = kithara_render::rt::StreamShape::new(
        NonZeroU32::new(32).expect("host block"),
        mock::SAMPLE_RATE,
    );
    let mut mixer = mock::MixerRig::new(DeckMixerConfig::default(), shape, &pools())
        .expect("real offline mixer");
    mixer
        .send(
            When::Next,
            DeckPart::Attach {
                slot: A,
                item_id: TrackId::allocate(),
                pcm: incumbent.pcm,
                segment: SegmentId::FIRST,
            },
        )
        .expect("native incumbent Attach");
    let mut pcm = [[0.0; 32]; 2];
    let [left, right] = &mut pcm;
    mixer
        .block(frame(0), [left, right])
        .expect("native incumbent attachment");
    while let Some(receipt) = mixer.ring.receipt() {
        let kithara_command::ScopedReceipt::Scope(_, receipt) = receipt else {
            panic!("incumbent scope");
        };
        assert!(matches!(receipt.outcome(), Outcome::Applied { .. }));
    }
    mixer
        .send(
            When::Next,
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        )
        .expect("native incumbent Start");
    let [left, right] = &mut pcm;
    mixer
        .block(frame(32), [left, right])
        .expect("incumbent produces PCM");
    assert!(pcm.iter().flatten().any(|sample| *sample != 0.0));
    while let Some(receipt) = mixer.ring.receipt() {
        let kithara_command::ScopedReceipt::Scope(_, receipt) = receipt else {
            panic!("incumbent scope");
        };
        assert!(matches!(receipt.outcome(), Outcome::Applied { .. }));
    }
    (mixer, pcm)
}

fn assert_incumbent_survives_release(
    newcomer: &mut Track,
    support: &mut Rig,
    mixer: &mut mock::MixerRig,
    pcm: &mut [[f32; 32]; 2],
) {
    for at in [96, 128, 160, 192, 224, 256] {
        with_mixer(mixer, &mut support.dispatcher, frame(at), |out| {
            newcomer.tick(frame(at), out);
        });
        let [left, right] = &mut *pcm;
        mixer
            .block(frame(at), [left, right])
            .expect("incumbent survives refused replacement");
        assert!(pcm.iter().flatten().any(|sample| *sample != 0.0));
        assert_eq!(
            mixer.ends.snapshot.read().slots[A.index()].state,
            SlotState::Playing
        );
        assert_eq!(
            mixer.ends.snapshot.read().slots[A.index()]
                .source
                .expect("incumbent source")
                .segment,
            SegmentId::FIRST
        );
        while let Some(receipt) = mixer.ring.receipt() {
            let kithara_command::ScopedReceipt::Scope(_, receipt) = receipt else {
                panic!("incumbent scope");
            };
            assert!(!receipt.batch().commands.iter().any(|part| matches!(
                part,
                DeckPart::Detach { .. }
                    | DeckPart::Returned(Returned::Pcm { .. })
                    | DeckPart::Replace { .. }
            )));
        }
    }
    assert!(!newcomer.attached());
    assert!(!newcomer.releasing);
}

fn settle_refused_replacement(
    newcomer: &mut Track,
    support: &mut Rig,
    mixer: &mut mock::MixerRig,
    replacement: Seq,
) {
    let mut refused = false;
    while let Some(receipt) = mixer.ring.receipt() {
        let kithara_command::ScopedReceipt::Scope(_, receipt) = receipt else {
            panic!("replacement scope");
        };
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        assert!(
            !batch
                .commands
                .iter()
                .any(|part| matches!(part, DeckPart::Detach { .. }))
        );
        if seq == replacement {
            assert!(matches!(outcome, Outcome::Rejected(Rejection::Stale)));
            refused = true;
        }
        with_mixer(mixer, &mut support.dispatcher, frame(64), |out| {
            newcomer.settle(
                TrackReceipt::Deck {
                    seq,
                    outcome: &outcome,
                    batch: &mut batch,
                },
                out,
            )
        });
    }
    assert!(refused);
    assert!(!newcomer.attached());
    assert_eq!(newcomer.attach_at, None);
    assert!(!newcomer.releasing);
}

fn assert_failed_replacement_keeps_incumbent(
    host_rate: bool,
    tick_before_receipt: bool,
    release_before_receipt: bool,
) {
    let mut support = rig();
    let (mut mixer, mut pcm) = playing_incumbent(&mut support);
    let mut newcomer = track(A);
    newcomer.slot = None;
    load(&mut newcomer, &mut support, Duration::ZERO);
    let opened = opened_fixture(&mut support, "failed-replacement-newcomer");
    let receipt = support.open(Ok(opened)).expect("native newcomer open");
    with_mixer(&mut mixer, &mut support.dispatcher, frame(64), |out| {
        newcomer.settle(TrackReceipt::Loaded(receipt), out)
    });
    assert_eq!(newcomer.status(), TrackStatus::Loaded);
    assert!(!newcomer.attached());
    let (sender, mut inbox) = lane();
    newcomer.lane = Some(sender);
    let group = with_mixer(&mut mixer, &mut support.dispatcher, frame(64), |out| {
        let at = When::At(frame(256));
        out.together_owned(at, |out| {
            newcomer.apply(TrackCommand::Seat { slot: A, at }, out)?;
            newcomer.apply(
                TrackCommand::Fade {
                    at,
                    settings: CrossfadeSettings::default(),
                    dir: FadeDir::In,
                },
                out,
            )?;
            Ok(())
        })
    });
    let replacement = match group {
        Ok(((), Some(seq))) => {
            newcomer.finish_group(Ok(seq));
            seq
        }
        Ok(((), None)) => panic!("native replacement group"),
        Err((error, mut parts)) => {
            newcomer.finish_group(Err(&mut parts));
            panic!("replacement admission: {error}");
        }
    };
    let segment = newcomer.segment;
    let source = newcomer
        .send_lane(
            if host_rate {
                LaneCommand::SetHostRate {
                    id: segment,
                    rate: mock::SAMPLE_RATE,
                }
            } else {
                LaneCommand::Segment {
                    id: segment,
                    from: Duration::ZERO,
                    speed: SpeedCurve::Constant(1.0),
                }
            },
            When::Next,
        )
        .expect("native source preparation");
    inbox.drain();
    let due = inbox
        .next_due(LaneFrame { segment, frame: 0 }, 1)
        .expect("native source refusal");
    assert_eq!(due.seq(), source);
    due.refuse(TrackFailureKind::Render);
    assert!(matches!(
        with_mixer(&mut mixer, &mut support.dispatcher, frame(64), |out| {
            newcomer.prepare_entry(frame(256), out)
        }),
        Err(PlayError::Lane(TrackFailureKind::Render))
    ));
    let failure = TrackStatus::Failed {
        at: frame(64),
        fault: PlaybackFault::Source(TrackFailureKind::Render),
    };
    if tick_before_receipt {
        with_mixer(&mut mixer, &mut support.dispatcher, frame(64), |out| {
            newcomer.tick(frame(64), out);
        });
        assert_eq!(newcomer.status(), failure);
    }
    with_mixer(&mut mixer, &mut support.dispatcher, frame(64), |out| {
        if release_before_receipt {
            newcomer.apply(TrackCommand::Release, out).map(drop)
        } else {
            out.supersede(A)
        }
    })
    .expect("cancel the pending replacement without touching its incumbent");
    let [left, right] = &mut pcm;
    mixer
        .block(frame(64), [left, right])
        .expect("native replacement rejection");
    assert!(pcm.iter().flatten().any(|sample| *sample != 0.0));
    settle_refused_replacement(&mut newcomer, &mut support, &mut mixer, replacement);
    with_mixer(&mut mixer, &mut support.dispatcher, frame(96), |out| {
        newcomer.tick(frame(96), out);
    });
    if release_before_receipt {
        assert_eq!(newcomer.status(), TrackStatus::Released);
    } else {
        assert_eq!(
            newcomer.status(),
            if tick_before_receipt {
                failure
            } else {
                TrackStatus::Failed {
                    at: frame(96),
                    fault: PlaybackFault::Source(TrackFailureKind::Render),
                }
            }
        );
        with_mixer(&mut mixer, &mut support.dispatcher, frame(96), |out| {
            newcomer.apply(TrackCommand::Release, out)
        })
        .expect("release the refused resource without detaching its incumbent");
    }
    assert_eq!(newcomer.status(), TrackStatus::Released);
    assert_incumbent_survives_release(&mut newcomer, &mut support, &mut mixer, &mut pcm);
}

fn assert_terminal_adoption_refusal(cancelled: bool, outdated: bool) {
    for tick_before_receipt in [false, true] {
        for release_before_receipt in [false, true] {
            let mut rig = rig();
            let (mut track, mut inbox) = loaded(&mut rig, A, "terminal-adoption");
            let deferred = cancelled || !outdated;
            let original = command_at_zero(
                &mut track,
                &mut rig,
                if deferred {
                    TrackCommand::PlayAfter { track: A }
                } else {
                    TrackCommand::Seek { to: Duration::ZERO }
                },
            )
            .expect("native adoption");
            let segment = track.segment;
            if cancelled {
                command_at_zero(&mut track, &mut rig, TrackCommand::Supersede);
                assert!(
                    track
                        .adopting
                        .iter()
                        .any(|adoption| adoption.cancelled && adoption.parked.is_some())
                );
            } else if !outdated {
                rig.with_outbox(|out| out.supersede(A))
                    .expect("live scope")
                    .expect("native stale basis");
            }
            inbox.drain();
            if deferred {
                inbox
                    .next_deferred()
                    .expect("parked source preparation")
                    .refuse(TrackFailureKind::Render);
            } else {
                inbox
                    .next_due(LaneFrame { segment, frame: 0 }, 1)
                    .expect("source preparation")
                    .refuse(TrackFailureKind::Render);
            }
            assert!(matches!(
                in_pass(&mut rig, frame(64), |out| track
                    .prepare_entry(frame(128), out)),
                Err(PlayError::Lane(TrackFailureKind::Render))
            ));
            if tick_before_receipt {
                in_pass(&mut rig, frame(64), |out| track.tick(frame(64), out));
                assert!(matches!(track.status(), TrackStatus::Failed { .. }));
            }
            let mut held = if outdated {
                let receipt = answer_next(&mut rig, Err(DeckRefusal::Outdated { slot: A }));
                assert_eq!(receipt.seq(), original);
                Answer::from(receipt)
            } else {
                let mut receipts = rig.block(frame(64), 0.0);
                let index = receipts
                    .iter()
                    .position(|receipt| receipt.seq == original)
                    .expect("actual stale adoption answer");
                let receipt = receipts.remove(index);
                assert!(
                    matches!(receipt.outcome, Outcome::Rejected(Rejection::Stale)),
                    "native stale adoption: cancelled={cancelled}, tick_before_receipt={tick_before_receipt}, release_before_receipt={release_before_receipt}, outcome={:?}",
                    receipt.outcome
                );
                receipt
            };
            if release_before_receipt {
                in_pass(&mut rig, frame(128), |out| {
                    track.apply(TrackCommand::Release, out)
                })
                .expect("explicit terminal release");
                let mut receipts = rig.block(frame(128), 0.0);
                settle(&mut track, &mut rig, &mut receipts);
                assert_eq!(track.status(), TrackStatus::Released);
                assert!(!track.attached());
            }
            let lane_count = track.lane_commands.len();
            in_pass(&mut rig, frame(192), |out| {
                track.settle(
                    TrackReceipt::Deck {
                        seq: held.seq,
                        outcome: &held.outcome,
                        batch: &mut held.batch,
                    },
                    out,
                )
            });
            assert!(track.adopting.is_empty());
            assert_eq!(track.newest_epoch_pending, None);
            assert_eq!(track.segment, segment);
            assert_eq!(track.lane_commands.len(), lane_count);
            in_pass(&mut rig, frame(256), |out| track.tick(frame(256), out));
            assert_eq!(track.segment, segment);
            assert_eq!(track.lane_commands.len(), lane_count);
            assert_eq!(track.newest_epoch_pending, None);
            if release_before_receipt {
                assert_eq!(track.status(), TrackStatus::Released);
            } else {
                assert_eq!(
                    track.status(),
                    TrackStatus::Failed {
                        at: frame(if tick_before_receipt { 64 } else { 256 }),
                        fault: PlaybackFault::Source(TrackFailureKind::Render),
                    }
                );
            }
            assert!(
                rig.block(frame(256), 0.0).is_empty(),
                "terminal receipt cannot enqueue a new Adopt"
            );
            inbox.drain();
            assert!(inbox.next_due(LaneFrame { segment, frame: 0 }, 1).is_none());
            assert!(
                inbox.next_deferred().is_none(),
                "cancelled terminal repeat cannot restore a Segment"
            );
        }
    }
}

#[kithara::test]
fn a_later_speed_does_not_replace_the_prepared_origin_law() {
    let mut rig = rig();
    let (mut track, mut inbox) = prepared_track();
    let curve = SpeedCurve::Approach(
        kithara_signal::ExponentialApproach::new(1.0, 0.9, 220.5).expect("finite curve"),
    );
    in_pass(&mut rig, frame(0), |out| {
        track.cue(Duration::ZERO, curve.clone(), out)
    })
    .expect("cue");
    let segment = track.segment;
    inbox.drain();
    inbox
        .next_due(LaneFrame { segment, frame: 0 }, 1)
        .expect("native segment")
        .apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: Some(segment),
        });
    track.settle_lane();
    track.resume = Some(SlotMark {
        session: frame(64),
        lane: LaneFrame { segment, frame: 64 },
        position: Duration::from_secs_f64(64.0 / f64::from(mock::SAMPLE_RATE.get())),
    });
    assert_eq!(
        track.prepared_speed(),
        None,
        "a consumed transient is not a fresh entry law"
    );
    track.resume = None;
    track
        .send_lane(
            LaneCommand::SetSpeed(SpeedCurve::Constant(1.5)),
            When::At(LaneFrame { segment, frame: 64 }),
        )
        .expect("later speed");
    assert_eq!(track.prepared_speed(), None);
    inbox.drain();
    drop(
        inbox
            .next_due(LaneFrame { segment, frame: 64 }, 1)
            .expect("later speed"),
    );
    track.settle_lane();
    assert_eq!(
        track.prepared_speed(),
        Some(&curve),
        "a rejected speed does not change the origin law"
    );
    let replacement = SpeedCurve::Constant(1.25);
    track
        .send_lane(
            LaneCommand::SetSpeed(replacement.clone()),
            When::At(LaneFrame { segment, frame: 0 }),
        )
        .expect("origin speed");
    assert_eq!(track.prepared_speed(), Some(&replacement));
}

#[kithara::test]
fn a_replay_entry_waits_for_native_adoption_after_lane_readiness() {
    let mut rig = rig();
    let (mut track, mut inbox) = prepared_track();
    track.status = TrackStatus::Ended { at: frame(480_000) };
    let curve = SpeedCurve::Approach(
        kithara_signal::ExponentialApproach::new(1.025, 1.025, 0.0).expect("exact replay curve"),
    );
    in_pass(&mut rig, frame(0), |out| {
        track.cue(Duration::ZERO, curve.clone(), out)
    })
    .expect("replay cue");
    let segment = track.segment;
    inbox.drain();
    inbox
        .next_due(LaneFrame { segment, frame: 0 }, 1)
        .expect("replay segment")
        .apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: Some(segment),
        });
    assert!(
        !in_pass(&mut rig, frame(0), |out| track
            .prepare_entry(frame(128), out))
        .expect("adoption pending")
    );
    assert_eq!(
        track.lane_commands.len(),
        1,
        "lane readiness cannot reopen the replay again"
    );
    let receipt = answer_next(&mut rig, Ok(()));
    answer_track(&mut track, &mut rig, receipt);
    assert_eq!(track.status, TrackStatus::Loaded);
    assert!(
        in_pass(&mut rig, frame(0), |out| track
            .prepare_entry(frame(128), out))
        .expect("adopted replay")
    );
    assert_eq!(track.prepared_speed(), Some(&curve));
    assert_eq!(track.lane_commands.len(), 1);
}

#[kithara::test]
fn an_older_native_ready_receipt_cannot_admit_the_replacement_segment() {
    let mut rig = rig();
    let (mut track, mut inbox) = prepared_track();
    in_pass(&mut rig, frame(0), |out| {
        track.cue(Duration::ZERO, SpeedCurve::Constant(1.0), out)
    })
    .expect("first cue");
    let old = track.segment;
    let curve = SpeedCurve::Approach(
        kithara_signal::ExponentialApproach::new(1.0, 0.9, 220.5).expect("replacement law"),
    );
    in_pass(&mut rig, frame(0), |out| {
        track.cue(Duration::ZERO, curve.clone(), out)
    })
    .expect("replacement cue");
    let current = track.segment;
    let bound = Bound::AtOrAfter(frame(128));
    assert_eq!(track.ready, None);
    assert_eq!(track.entry(bound), None);
    track.status = TrackStatus::Loading;
    assert_eq!(track.entry(bound), None);
    assert_ne!(old, current);
    inbox.drain();
    inbox
        .next_due(
            LaneFrame {
                segment: old,
                frame: 0,
            },
            1,
        )
        .expect("older native segment")
        .apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: Some(old),
        });
    assert!(
        !in_pass(&mut rig, frame(0), |out| track
            .prepare_entry(frame(128), out))
        .expect("old receipt intake")
    );
    assert_eq!(track.ready, Some(old));
    assert_eq!(track.entry(bound), None);
    track.status = TrackStatus::Loaded;
    assert_eq!(track.entry(bound), None);
    track.status = TrackStatus::Loading;
    assert!(track.snapshot().pending_lane);
    assert_eq!(track.prepared_speed(), Some(&curve));
    inbox
        .next_due(
            LaneFrame {
                segment: current,
                frame: 0,
            },
            1,
        )
        .expect("current native segment")
        .apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: Some(current),
        });
    assert!(
        in_pass(&mut rig, frame(0), |out| track
            .prepare_entry(frame(128), out))
        .expect("current receipt intake")
    );
    assert_eq!(track.ready, Some(current));
    assert_eq!(track.entry(bound), Some(frame(128)));
    track.status = TrackStatus::Loaded;
    assert_eq!(track.entry(bound), Some(frame(128)));
    assert!(!track.snapshot().pending_lane);
}

#[cfg(feature = "usdt")]
#[kithara::test]
#[case::constant(SpeedCurve::Constant(1.5))]
#[case::ramp(SpeedCurve::Ramp {
    to: 1.5,
    frames: std::num::NonZeroU64::new(441).expect("nonempty ramp"),
})]
#[case::steps(SpeedCurve::Steps(Arc::from([(0, 1.25), (441, 1.5)])))]
fn a_queued_speed_publishes_its_target_and_sequence_once(#[case] curve: SpeedCurve) {
    let trace = kithara_test_utils::test::usdt::scope();
    let mut track = track(A);
    let (sender, _inbox) = channel(
        ChannelConfig::builder()
            .capacity(NonZeroUsize::new(1).expect("one batch"))
            .build(),
    );
    track.lane = Some(sender);
    track.mark = Some(SlotMark {
        session: frame(128),
        lane: LaneFrame {
            segment: SegmentId::FIRST,
            frame: 64,
        },
        position: Duration::ZERO,
    });
    let seq = track
        .send_lane(LaneCommand::SetSpeed(curve.clone()), When::Next)
        .expect("accepted speed");
    let requests = trace.events_of("rate_requested");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].field("request_revision"), Some(seq.get()));
    assert_eq!(
        requests[0].field("target_rate_bits"),
        Some(u64::from(1.5_f32.to_bits())),
    );
    assert_eq!(requests[0].field("session_frame"), Some(128));
    assert!(matches!(
        track.send_lane(LaneCommand::SetSpeed(curve), When::Next),
        Err(PlayError::Full(_)),
    ));
    assert_eq!(trace.events_of("rate_requested").len(), 1);
    drop(trace);
}

#[kithara::test]
fn a_ready_paused_segment_keeps_its_cue_over_an_old_slot_snapshot() {
    let mut rig = rig();
    let (mut track, _) = prepared_track();
    let (sender, mut inbox) = channel(ChannelConfig::builder().build());
    track.lane = Some(sender);
    track.status = TrackStatus::Paused { at: Position::ZERO };
    let to = Duration::from_secs(2);
    let _ = command_at_zero(&mut track, &mut rig, TrackCommand::Seek { to });
    let segment = track.segment;
    inbox.drain();
    inbox
        .next_due(LaneFrame { segment, frame: 0 }, 1)
        .expect("cue segment")
        .apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: Some(segment),
        });
    track.settle_lane();
    let output = mock::output(None).get();
    let snapshot = crate::DeckSnapshot::new(DeckMixerConfig::default());
    let pass = crate::DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: frame(0),
        delivery: FrameCount::new(0),
        output: &output,
        deck: &snapshot,
    };
    let deck = std::ops::DerefMut::deref_mut(&mut rig);
    let mut scope = deck.ring.scope(deck.scope).expect("live deck scope");
    let out = Outbox::new(&mut scope, &mut deck.dispatcher).in_pass(pass);
    track.observe(&out);
    assert_eq!(track.snapshot().position, to);
    assert_eq!(track.snapshot().status, TrackStatus::Paused { at: to });
}

#[kithara::test]
#[case::applied(true)]
#[case::rejected(false)]
fn an_alignment_uses_one_credit_and_settles_every_part(#[case] applied: bool) {
    let mut rig = rig();
    let (mut track, _) = prepared_track();
    let (sender, mut inbox) = channel(
        ChannelConfig::builder()
            .capacity(NonZeroUsize::new(1).expect("one batch"))
            .build(),
    );
    track.lane = Some(sender);
    track.declick = FrameCount::new(32);
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: SegmentId::FIRST,
            frame: 0,
        },
        position: Duration::ZERO,
    });
    let to = Duration::from_secs(2);
    let landing = frame(i64::try_from(track.declick.get()).expect("declick fits"));
    let seq = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Align {
            to,
            speed: SpeedCurve::Constant(1.5),
            at: landing,
        },
    )
    .expect("alignment batch");
    assert_eq!(track.lane.as_ref().expect("lane").available(), 0);
    assert_eq!(track.snapshot().speed, 1.0);
    inbox.drain();
    let at = LaneFrame {
        segment: SegmentId::FIRST,
        frame: 0,
    };
    let due = inbox.next_due(at, 1).expect("one alignment");
    assert_eq!(due.seq(), seq);
    assert!(
        matches!(due.commands(), [LaneCommand::SetSpeed(SpeedCurve::Constant(1.5)), LaneCommand::Jump { to: target }] if *target == to)
    );
    if applied {
        due.apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: None,
        });
    } else {
        drop(due);
    }
    assert!(inbox.next_due(at, 1).is_none());
    track.settle_lane();
    assert_eq!(track.lane_commands.len(), 2);
    assert!(
        track
            .lane_commands
            .iter()
            .all(|operation| operation.seq == seq && operation.applied() == Some(applied))
    );
    let receipt = track.speed_receipt().expect("batch verdict");
    assert!(match receipt {
        Settled::Applied { seq: named, .. } => applied && named == seq,
        Settled::Rejected {
            seq: named,
            reason: Rejection::Unanswered,
        } => !applied && named == seq,
        _ => false,
    });
    assert!(track.speed_receipt().is_none());
    let (planned, speed) = track
        .planned(landing, mock::SAMPLE_RATE)
        .expect("planned landing");
    assert_eq!(
        planned,
        if applied {
            to
        } else {
            Duration::from_secs_f64(32.0 / f64::from(mock::SAMPLE_RATE.get()))
        }
    );
    assert_eq!(speed, if applied { 1.5 } else { 1.0 });
    assert_eq!(track.snapshot().speed, if applied { 1.5 } else { 1.0 });
    assert_eq!(track.lane.as_ref().expect("lane").available(), 1);
}
