use super::*;

fn assert_consumed_speed_twice(track: &Track, expected: f32) {
    for _ in 0..2 {
        assert_eq!(
            track.consumed_speed().expect("consumed speed").to_bits(),
            expected.to_bits()
        );
    }
}

#[kithara::test]
fn speed_projection_keeps_frame_admission_fresh_after_warming() {
    let (mut track, _inbox) = prepared_track();
    assert_consumed_speed_twice(&track, 1.0);
    track.status = TrackStatus::Playing { since: frame(0) };
    assert!(matches!(track.consumed_speed(), Err(PlayError::Untimed)));
    let mark = SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: track.segment.id,
            frame: 0,
        },
        position: Duration::ZERO,
    };
    track.mark = Some(mark);
    assert_consumed_speed_twice(&track, 1.0);
    let foreign = SlotMark {
        lane: LaneFrame {
            segment: track.segment.id.next(),
            frame: 0,
        },
        ..mark
    };
    track.resume = Some(foreign);
    assert!(matches!(track.consumed_speed(), Err(PlayError::Untimed)));
    track.resume = None;
    track.mark = Some(foreign);
    assert!(matches!(track.consumed_speed(), Err(PlayError::Untimed)));
}

#[kithara::test]
fn speed_projection_keeps_same_frame_submissions_visible() {
    let (mut track, _inbox) = prepared_track();
    assert_consumed_speed_twice(&track, 1.0);
    track
        .send_lane(
            LaneCommand::SetSpeed(SpeedCurve::Constant(1.75)),
            When::At(LaneFrame {
                segment: track.segment.id,
                frame: 0,
            }),
        )
        .expect("same-frame speed");
    assert_consumed_speed_twice(&track, 1.75);
}

#[kithara::test]
fn speed_projection_observes_applied_and_refused_receipts() {
    let (mut track, mut inbox) = prepared_track();
    let at = LaneFrame {
        segment: track.segment.id,
        frame: 0,
    };
    track
        .send_lane(
            LaneCommand::SetSpeed(SpeedCurve::Constant(1.75)),
            When::Next,
        )
        .expect("unresolved speed");
    assert_consumed_speed_twice(&track, 1.0);
    inbox.drain();
    inbox
        .next_due(at, 1)
        .expect("due next-frame speed")
        .apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: None,
        });
    track.settle_lane();
    assert_consumed_speed_twice(&track, 1.75);
    track
        .send_lane(
            LaneCommand::SetSpeed(SpeedCurve::Constant(2.0)),
            When::At(at),
        )
        .expect("second speed");
    assert_consumed_speed_twice(&track, 2.0);
    inbox.drain();
    inbox
        .next_due(at, 1)
        .expect("due refused speed")
        .refuse(kithara_audio::TrackFailureKind::Render);
    track.settle_lane();
    assert_consumed_speed_twice(&track, 1.75);
}

#[kithara::test]
fn speed_projection_tracks_frame_segment_and_initial_rate() {
    let (mut track, _inbox) = prepared_track();
    track.segment.speed = 1.25;
    track
        .send_lane(
            LaneCommand::SetSpeed(SpeedCurve::Ramp {
                to: 2.0,
                frames: std::num::NonZeroU64::new(40).expect("ramp length"),
            }),
            When::At(LaneFrame {
                segment: track.segment.id,
                frame: 0,
            }),
        )
        .expect("relative ramp");
    assert_consumed_speed_twice(&track, 1.25);
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(20),
        lane: LaneFrame {
            segment: track.segment.id,
            frame: 20,
        },
        position: Duration::from_micros(20),
    });
    assert_consumed_speed_twice(&track, 1.625);
    track.segment = Segment::issued(
        track.segment.id.next(),
        track.segment.origin,
        track.segment.speed,
    );
    track.mark.as_mut().expect("consumed mark").lane.segment = track.segment.id;
    assert_consumed_speed_twice(&track, 1.25);
    track.segment.speed = 1.5;
    assert_consumed_speed_twice(&track, 1.5);
}

#[kithara::test]
fn speed_projection_preserves_future_jump_overflow_after_declick_changes() {
    let (mut track, _inbox) = prepared_track();
    track.geometry.declick = FrameCount::new(0);
    track
        .send_lane(
            LaneCommand::Jump { to: Duration::ZERO },
            When::At(LaneFrame {
                segment: track.segment.id,
                frame: u64::MAX,
            }),
        )
        .expect("future jump");
    assert_consumed_speed_twice(&track, 1.0);
    track.geometry.declick = FrameCount::new(1);
    for _ in 0..2 {
        assert!(matches!(track.consumed_speed(), Err(PlayError::Untimed)));
    }
    track.geometry.declick = FrameCount::new(0);
    assert_consumed_speed_twice(&track, 1.0);
}

#[kithara::test]
#[case::constant(SpeedCurve::Constant(1.75), 1.75)]
#[case::approach(SpeedCurve::Approach(kithara_signal::ExponentialApproach::new(1.5, 2.0, 100.0).expect("approach")), (2.0 - 0.5 * (-0.1_f64).exp()).to_f32().expect("finite approach speed"))]
#[case::approach_steps(SpeedCurve::ApproachSteps(Arc::from(vec![(0, kithara_signal::ExponentialApproach::new(1.5, 2.0, 100.0).expect("first approach")), (8, kithara_signal::ExponentialApproach::new(1.75, 1.0, 0.0).expect("instant approach"))])), 1.0)]
#[case::absolute_steps(SpeedCurve::Steps(Arc::from(vec![(0, 1.75), (12, 2.0)])), 1.75)]
#[case::inherited_steps(SpeedCurve::Steps(Arc::from(vec![(12, 2.0)])), 1.1)]
#[case::inherited_ramp(SpeedCurve::Ramp { to: 2.0, frames: std::num::NonZeroU64::new(40).expect("ramp length") }, 1.325)]
fn consumed_speed_preserves_the_origin_needed_by_each_curve(
    #[case] curve: SpeedCurve,
    #[case] expected: f32,
) {
    let (mut track, _inbox) = prepared_track();
    for (at, curve) in [
        (1_000, SpeedCurve::Constant(3.0)),
        (
            0,
            SpeedCurve::Ramp {
                to: 2.0,
                frames: std::num::NonZeroU64::new(100).expect("inherited ramp length"),
            },
        ),
        (10, curve),
    ] {
        track
            .send_lane(
                LaneCommand::SetSpeed(curve),
                When::At(LaneFrame {
                    segment: track.segment.id,
                    frame: at,
                }),
            )
            .expect("scheduled curve");
    }
    let rejected = track
        .send_lane(
            LaneCommand::SetSpeed(SpeedCurve::Constant(3.5)),
            When::At(LaneFrame {
                segment: track.segment.id,
                frame: 15,
            }),
        )
        .expect("accepted before rejection");
    track
        .lane_history
        .commands
        .iter_mut()
        .find(|operation| operation.seq == rejected)
        .expect("recorded rejected curve")
        .applied = Some(false);
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(20),
        lane: LaneFrame {
            segment: track.segment.id,
            frame: 20,
        },
        position: Duration::from_millis(20),
    });
    assert_eq!(
        track.snapshot().speed.to_bits(),
        expected.to_bits(),
        "only the consumed curve and its inherited origin determine speed"
    );
}

#[kithara::test(tokio)]
async fn load_track_propagates_host_sample_rate() {
    let host_rate = 88_200u32;
    let rate = NonZeroU32::new(host_rate).expect("host rate");
    let dir = TestTempDir::new();
    let path = dir.path().join("rate.wav");
    std::fs::write(
        &path,
        kithara_test_fixtures::assets::fixed_constant_wav_cd_half_1024().bytes(),
    )
    .expect("float WAV fixture");
    let pools = pools();
    let prep = crate::ResourcePrep::builder()
        .worker(crate::PlayWorker::new(
            crate::PlayWorkerConfig::builder(pools.clone()).build(),
        ))
        .build();
    let output = crate::SessionOutputView::new(rate);
    let config = ResourceConfig::for_src(ResourceSrc::Path(path))
        .store(
            AssetStore::builder(pools)
                .backend(kithara_assets::StorageBackend::Memory)
                .build(),
        )
        .build();
    let prepared = prep
        .prepare(config, &output.get())
        .expect("prepared source");
    let mut rig = DeckRig::new(DeckMixerConfig::default()).expect("deck scope");
    let loaded = rig
        .load_fixture(
            &prep.worker,
            ResourceLoad::new(prepared, Box::new(AudioObserverSlot::default().relay())),
            Duration::ZERO,
        )
        .await
        .expect("worker opened source");
    assert_eq!(loaded.opened.pcm.spec().sample_rate.get(), host_rate);
}

#[kithara::test]
fn consumed_speed_matches_the_full_piecewise_schedule() {
    for shift in 0_u64..24 {
        let (mut track, _inbox) = prepared_track();
        let mut schedule = Vec::new();
        for index in 0..12 {
            let at = (index / 2 * 17 + shift * 5) % 97;
            let target = if index % 2 == 0 { 1.25_f32 } else { 1.75 };
            let approach = kithara_signal::ExponentialApproach::new(1.5, f64::from(target), 19.0)
                .expect("finite approach");
            let curve = match (index + shift) % 6 {
                0 => SpeedCurve::Constant(target),
                1 => SpeedCurve::Ramp {
                    to: target,
                    frames: std::num::NonZeroU64::new(19).expect("ramp frames"),
                },
                2 => SpeedCurve::Approach(approach),
                3 => SpeedCurve::ApproachSteps(Arc::from(vec![(0, approach)])),
                4 => SpeedCurve::Steps(Arc::from(vec![(0, 1.5), (7, target)])),
                _ => SpeedCurve::Steps(Arc::from(vec![(7, target)])),
            };
            let seq = track
                .send_lane(
                    LaneCommand::SetSpeed(curve.clone()),
                    When::At(LaneFrame {
                        segment: track.segment.id,
                        frame: at,
                    }),
                )
                .expect("scheduled curve");
            if index % 5 == 0 {
                track
                    .lane_history
                    .commands
                    .last_mut()
                    .expect("recorded curve")
                    .applied = Some(false);
            } else {
                schedule.push((at, seq, curve));
            }
        }
        schedule.sort_by_key(|(at, seq, _)| (*at, *seq));
        track.status = TrackStatus::Playing { since: frame(0) };
        for consumed in 0..128 {
            let base = SpeedCurve::Constant(track.segment.speed);
            let mut curve = &base;
            let mut origin = f64::from(track.segment.speed);
            let mut start = 0;
            for (at, _, next) in schedule.iter().filter(|(at, _, _)| *at <= consumed) {
                origin = curve_speed(curve, origin, at - start).expect("prior curve boundary");
                curve = next;
                start = *at;
            }
            let expected = curve_speed(curve, origin, consumed - start)
                .expect("full schedule rate")
                .to_f32()
                .expect("finite f32 rate");
            track.mark = Some(SlotMark {
                session: frame(consumed.to_i64().expect("consumed frame fits i64")),
                lane: LaneFrame {
                    segment: track.segment.id,
                    frame: consumed,
                },
                position: Duration::from_micros(consumed),
            });
            for read in 0..2 {
                assert_eq!(
                    track.consumed_speed().expect("consumed rate").to_bits(),
                    expected.to_bits(),
                    "schedule {shift} at frame {consumed}, read {read}"
                );
            }
        }
    }
}
