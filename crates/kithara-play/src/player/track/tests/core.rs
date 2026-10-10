pub(super) use kithara_assets::AssetStore;
pub(super) use kithara_audio::AudioObserverSlot;
pub(super) use kithara_command::{ChannelConfig, Inbox, Port, channel};
pub(super) use kithara_platform::sync::Arc;
pub(super) use kithara_render::{
    DispatcherCommand, LaneFrame, LoadRefusal, Loaded, Open,
    bridge::{DeckEvent, DeckMixSettings, SlotSnapshot},
    rt::DeckMixerConfig,
};
pub(super) use kithara_signal::AudioSpec;
pub(super) use kithara_test_utils::{TestTempDir, kithara};
pub(super) use kithara_warp::{SpeedCurve, StretchKind};

use super::*;
pub(super) use crate::{
    Player, ResourceConfig, ResourceSrc,
    mock::{self, DeckRig},
    player::factory::Track as _,
    test_pools::{TestPools, pools},
};

pub(super) const A: Slot = Slot::new(0);
pub(super) const B: Slot = Slot::new(1);

#[kithara::test]
#[case::remaining_equals_window(157.0, 162.0, 1.0, 5.0, true)]
#[case::remaining_below_window(160.0, 162.0, 1.0, 5.0, true)]
#[case::far_from_end(100.0, 162.0, 1.0, 5.0, false)]
#[case::double_speed_halves_the_session_time_left(152.0, 162.0, 2.0, 5.0, true)]
#[case::double_speed_media_tail_is_not_yet_due(150.0, 162.0, 2.0, 5.0, false)]
#[case::half_speed_media_tail_is_too_long(158.0, 162.0, 0.5, 5.0, false)]
#[case::stopped_track_never_ends(161.0, 162.0, 0.0, 5.0, false)]
#[case::zero_window_only_at_the_end(161.9, 162.0, 1.0, 0.0, false)]
#[case::zero_position_rejected(0.0, 162.0, 1.0, 5.0, false)]
#[case::zero_duration_rejected(10.0, 0.0, 1.0, 5.0, false)]
fn ends_within_cases(
    #[case] pos: f64,
    #[case] dur: f64,
    #[case] rate: f64,
    #[case] window: f32,
    #[case] expected: bool,
) {
    let mut track = track(A);
    track.duration = (dur > 0.0).then(|| Duration::from_secs_f64(dur));
    track.segment_speed = rate.to_f32().expect("fixture speed fits f32");
    track.status = if rate == 0.0 {
        TrackStatus::Paused {
            at: Duration::from_secs_f64(pos),
        }
    } else {
        TrackStatus::Playing { since: frame(0) }
    };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: SegmentId::FIRST,
            frame: 0,
        },
        position: Duration::from_secs_f64(pos),
    });
    let window_end = frame(
        (f64::from(window) * f64::from(mock::SAMPLE_RATE.get()))
            .to_i64()
            .expect("fixture window fits the session timeline"),
    );
    assert_eq!(
        track
            .planned_end(mock::SAMPLE_RATE)
            .expect("timed track end")
            .is_some_and(|end| end <= window_end),
        expected
    );
}

pub(super) struct Rig {
    deck: DeckRig<TestPools>,
    sources: Vec<TestTempDir>,
    runtime: kithara_platform::tokio::runtime::Runtime,
}

impl std::ops::Deref for Rig {
    type Target = DeckRig<TestPools>;

    fn deref(&self) -> &Self::Target {
        &self.deck
    }
}

impl std::ops::DerefMut for Rig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.deck
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct Answer {
    seq: Seq,
    outcome: Outcome<DeckProtocol>,
    #[field(get, vis = "")]
    batch: Batch<DeckProtocol>,
}

impl From<Receipt<DeckProtocol>> for Answer {
    fn from(receipt: Receipt<DeckProtocol>) -> Self {
        let seq = receipt.seq();
        let (outcome, batch) = receipt.into();
        Self {
            seq,
            outcome,
            batch,
        }
    }
}

impl Rig {
    pub(super) fn block(&mut self, at: SessionFrame, stopped_at: f64) -> Vec<Answer> {
        self.deck
            .block(at, stopped_at)
            .expect("mock deck block")
            .into_iter()
            .map(Answer::from)
            .collect()
    }

    pub(super) fn end(&mut self, slot: Slot, at: SessionFrame) -> Vec<Answer> {
        self.deck
            .end(slot, at)
            .expect("mock deck end")
            .into_iter()
            .map(Answer::from)
            .collect()
    }
}
pub(super) type Track = PlayerImpl<TestPools>;

pub(super) fn rig() -> Rig {
    Rig {
        deck: DeckRig::new(DeckMixerConfig::default()).expect("deck scope opens"),
        sources: Vec::new(),
        runtime: kithara_platform::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fixture runtime starts"),
    }
}

pub(super) fn frame(value: i64) -> SessionFrame {
    SessionFrame::new(value)
}

pub(super) fn track(slot: Slot) -> Track {
    let settings = TrackSettings::builder()
        .speed(1.0)
        .keylock(false)
        .backend(StretchKind::default())
        .build();
    PlayerImpl::new(PlayerConfig {
        item: TrackId::allocate(),
        slot: Some(slot),
        settings,
    })
    .expect("unity speed is a valid track speed")
}

fn unseated_track() -> Track {
    let mut held = track(A);
    held.slot = None;
    held
}

pub(super) fn item() -> ResourceLoad<TestPools> {
    let src = ResourceSrc::parse("https://example.com/song.mp3").expect("valid test source");
    let config = ResourceConfig::for_src(src)
        .store(AssetStore::builder(pools()).build())
        .worker(crate::PlayWorker::new(
            crate::PlayWorkerConfig::builder(pools()).build(),
        ))
        .host_sample_rate(mock::SAMPLE_RATE)
        .build();
    ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()))
}

pub(super) fn lane() -> (Sender<LaneProtocol>, Inbox<LaneProtocol>) {
    channel(ChannelConfig::builder().build())
}

pub(super) fn answer_next(rig: &mut Rig, answer: Result<(), DeckRefusal>) -> Receipt<DeckProtocol> {
    rig.deck
        .ring
        .publish()
        .expect("the deck publication succeeds");
    rig.deck.inbox.drain();
    {
        let mut level = rig
            .deck
            .inbox
            .scope(rig.deck.scope)
            .expect("live deck scope");
        if let Some(due) = level.next_due(frame(0), 1) {
            match answer {
                Ok(()) => due.apply(()),
                Err(refusal) => due.refuse(refusal),
            }
        }
    }
    match rig.deck.ring.receipt().expect("one terminal deck receipt") {
        kithara_command::ScopedReceipt::Scope(scope, receipt) => {
            assert_eq!(scope, rig.deck.scope);
            receipt
        }
        receipt => panic!("unexpected scope receipt: {receipt:?}"),
    }
}

pub(super) fn answer_track(
    track: &mut Track,
    rig: &mut Rig,
    receipt: Receipt<DeckProtocol>,
) -> Settled {
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    rig.with_outbox(|out| {
        track.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    })
    .expect("live deck scope")
}

pub(super) fn prepared_track() -> (Track, Inbox<LaneProtocol>) {
    let (sender, inbox) = lane();
    let mut track = track(A);
    track.lane = Some(sender);
    track.status = TrackStatus::Loaded;
    track.attach_at = Some(frame(0));
    track.ready = Some(SegmentId::FIRST);
    (track, inbox)
}

pub(super) fn in_pass<R>(
    rig: &mut Rig,
    now: SessionFrame,
    run: impl FnOnce(&mut Outbox<'_, TestPools>) -> R,
) -> R {
    let output = mock::output(None).get();
    let deck = crate::DeckSnapshot::default();
    let pass = crate::DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now,
        delivery: FrameCount::new(0),
        output: &output,
        deck: &deck,
    };
    let mut scope = rig
        .deck
        .ring
        .scope(rig.deck.scope)
        .expect("live deck scope");
    let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher).in_pass(pass);
    run(&mut out)
}

pub(super) fn command_at_zero(
    track: &mut Track,
    rig: &mut Rig,
    command: TrackCommand<TestPools>,
) -> Option<Seq> {
    in_pass(rig, frame(0), |out| track.apply(command, out)).expect("admitted owner command")
}

#[kithara::test]
#[case::applied(true)]
#[case::rejected(false)]
fn a_repeated_receipt_is_a_duplicate_and_changes_nothing(#[case] applied: bool) {
    let mut rig = rig();
    let (mut track, _inbox) = prepared_track();
    let seq = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    )
    .expect("scheduled start");
    let receipt = answer_next(
        &mut rig,
        if applied {
            Ok(())
        } else {
            Err(DeckRefusal::Outdated { slot: A })
        },
    );
    let (outcome, mut batch) = receipt.into();
    rig.with_outbox(|out| {
        track.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    })
    .expect("live scope");
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Pause {
            at: When::At(frame(0)),
        },
    );
    let before = track.snapshot();
    let pending = track.playback_commands.len();

    let replay = rig
        .with_outbox(|out| {
            track.settle(
                TrackReceipt::Deck {
                    seq,
                    outcome: &outcome,
                    batch: &mut batch,
                },
                out,
            )
        })
        .expect("live scope");

    assert!(matches!(replay, Settled::Pending));
    assert_eq!(track.snapshot().status, before.status);
    assert_eq!(track.snapshot().speed, before.speed);
    assert_eq!(track.snapshot().position, before.position);
    assert_eq!(track.playback_commands.len(), pending);
    assert_eq!(pending, 1);
}

#[kithara::test]
fn a_receipt_skipping_a_phase_is_refused() {
    let mut rig = rig();
    let (mut track, _inbox) = prepared_track();
    track.ready = None;
    assert_eq!(
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::Play {
                at: When::At(frame(0))
            }
        ),
        None
    );
    let pending = track.play;
    rig.with_outbox(|out| {
        out.deck(
            When::Next,
            vec![DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            }],
        )
    })
    .expect("live scope")
    .expect("foreign batch admitted");
    let receipt = answer_next(&mut rig, Ok(()));

    assert!(matches!(
        answer_track(&mut track, &mut rig, receipt),
        Settled::Pending
    ));
    assert_eq!(track.status, TrackStatus::Loaded);
    assert_eq!(track.play, pending);
    assert_eq!(pending, Some(When::At(frame(0))));
    assert!(track.playback_commands.is_empty());
}

#[kithara::test]
fn entering_arms_the_member_that_reached_presentation() {
    let mut rig = rig();
    let (mut track, _inbox) = prepared_track();
    assert_eq!(track.status, TrackStatus::Loaded, "Ready alone is silent");
    let seq = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    )
    .expect("scheduled start");
    assert_eq!(
        track.status,
        TrackStatus::Loaded,
        "sending Start cannot promote Playing"
    );
    let receipt = answer_next(&mut rig, Ok(()));
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(answer_track(&mut track, &mut rig, receipt), Settled::Applied { seq: answered, at } if answered == seq && at == frame(0))
    );
    let TrackStatus::Playing { since } = track.status else {
        panic!("matching Applied starts the track")
    };
    assert!(i64::from(since).abs() < 1);
    assert_eq!(track.play, None);
    assert!(track.playback_commands.is_empty());
}

#[kithara::test]
fn a_rejected_launch_leaves_the_member_silent() {
    let mut rig = rig();
    let (mut track, _inbox) = prepared_track();
    let pools = pools();
    let dir = TestTempDir::new();
    let path = dir.path().join("rejected.wav");
    mock::write_pcm_wav(
        &path,
        &vec![0.5; 8_192],
        AudioSpec::new(2, mock::SAMPLE_RATE),
    )
    .expect("audible source fixture");
    let worker = crate::PlayWorker::new(crate::PlayWorkerConfig::builder(pools.clone()).build());
    let prep = crate::ResourcePrep::builder().worker(worker).build();
    let config: ResourceConfig<TestPools> = prep
        .prepare(
            ResourceConfig::for_src(ResourceSrc::Path(path))
                .store(
                    AssetStore::builder(pools.clone())
                        .backend(kithara_assets::StorageBackend::Memory)
                        .build(),
                )
                .build(),
            &mock::output(None).get(),
        )
        .expect("prepared source");
    let load = ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()));
    let (_control, inbox) = load.lane_channel().expect("lane channel");
    let (opened, _lane, _) = rig
        .runtime
        .block_on(load.open(Duration::ZERO, TrackSettings::default().lane_start(), inbox))
        .expect("URI source opens");
    let shape = kithara_render::rt::StreamShape::new(
        NonZeroU32::new(128).expect("block frames"),
        mock::SAMPLE_RATE,
    );
    let mut mixer =
        mock::MixerRig::new(DeckMixerConfig::default(), shape, &pools).expect("offline mixer");
    mixer
        .send(
            When::Next,
            DeckPart::Attach {
                slot: A,
                pcm: opened.pcm,
                segment: SegmentId::FIRST,
            },
        )
        .expect("attach audible lane");
    let mut pcm = [[0.0; 128]; 2];
    let [left, right] = &mut pcm;
    mixer
        .block(frame(0), [left, right])
        .expect("loaded host block");
    assert!(mixer.ring.receipt().is_some());
    let output = mock::output(Some(shape)).get();
    let snapshot = crate::DeckSnapshot::default();
    let pass = crate::DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: frame(128),
        delivery: FrameCount::new(0),
        output: &output,
        deck: &snapshot,
    };
    let seq = {
        let mut scope = mixer.ring.scope(mixer.scope).expect("live scope");
        let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher).in_pass(pass);
        let seq = track
            .apply(
                TrackCommand::Play {
                    at: When::At(frame(256)),
                },
                &mut out,
            )
            .expect("scheduled start")
            .expect("start sequence");
        out.deck(
            When::At(frame(192)),
            vec![DeckPart::Stop {
                slot: A,
                fade: Fade::Declick,
            }],
        )
        .expect("intervening slot operation");
        seq
    };
    let [left, right] = &mut pcm;
    mixer
        .block(frame(128), [left, right])
        .expect("slot basis changes before launch");
    assert!(pcm.iter().flatten().all(|sample| *sample == 0.0));
    let [left, right] = &mut pcm;
    mixer
        .block(frame(256), [left, right])
        .expect("rejected launch host block");
    let _ = mixer.ring.receipt().expect("stop applied");
    let kithara_command::ScopedReceipt::Scope(_, receipt) =
        mixer.ring.receipt().expect("start rejected")
    else {
        panic!("deck reply");
    };
    assert_eq!(receipt.seq(), seq);
    let (outcome, mut batch) = receipt.into();
    assert!(matches!(outcome, Outcome::Rejected(Rejection::Stale)));
    let settled = {
        let mut scope = mixer.ring.scope(mixer.scope).expect("live scope");
        let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher);
        track.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            &mut out,
        )
    };
    assert!(matches!(settled, Settled::Rejected { seq: answered, .. } if answered == seq));
    assert_eq!(track.status, TrackStatus::Loaded);
    assert_eq!(track.play, None);
    assert!(track.playback_commands.is_empty());
    assert_eq!(rig.deck.mixer.held(A), None);
    assert!(pcm.iter().flatten().all(|sample| *sample == 0.0));
    assert_eq!(
        mixer.ends.snapshot.read().slots[0].state,
        SlotState::Stopped
    );
    mixer
        .send(
            When::Next,
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        )
        .expect("valid launch");
    let [left, right] = &mut pcm;
    mixer
        .block(frame(384), [left, right])
        .expect("audible control block");
    assert!(pcm.iter().flatten().any(|sample| *sample != 0.0));
}

#[kithara::test]
fn an_installed_lane_the_owner_refuses_is_dropped_and_reported_cancelled() {
    let mut rig = rig();
    let mut track = track(A);
    let load = load(&mut track, &mut rig, Position::ZERO);
    let opened = opened_fixture(&mut rig, "track");
    let lane = opened.lane;
    let receipt = rig.open(Ok(opened)).expect("one opened source");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live scope");
    let receipt = answer_next(&mut rig, Err(DeckRefusal::Outdated { slot: A }));
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();

    assert!(
        matches!(rig.with_outbox(|out| track.settle(TrackReceipt::Deck {
        seq, outcome: &outcome, batch: &mut batch,
    }, out)).expect("live scope"), Settled::Rejected { seq: answered, .. } if answered == load)
    );
    assert_eq!(track.status(), TrackStatus::Released);
    assert!(track.loading.is_none());
    assert!(track.attaching.is_none());
    assert!(track.play.is_none());
    assert!(track.ready.is_none());
    assert!(
        batch.commands.is_empty(),
        "the refused consumer is returned to its owner"
    );
    rig.deck.opens.drain();
    let due = rig.deck.opens.next_due((), 1).expect("one lane release");
    assert!(
        matches!(due.commands(), [DispatcherCommand::Release(released)]
        if *released == lane)
    );
    due.apply(Dispatched::Released);
    assert!(rig.deck.opens.next_due((), 1).is_none());
    let receipt = rig
        .deck
        .dispatcher
        .receipts()
        .next()
        .expect("release answer");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live scope");
    assert!(track.lane.is_none());
    assert!(track.lane_id.is_none());
    rig.with_outbox(|out| track.tick(frame(0), out))
        .expect("live scope");
    assert!(matches!(
        rig.with_outbox(|out| track.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &Outcome::Applied {
                    at: frame(0),
                    data: ()
                },
                batch: &mut batch,
            },
            out
        ))
        .expect("live scope"),
        Settled::Pending
    ));
    assert_eq!(track.status(), TrackStatus::Released);
    rig.deck.ring.publish().expect("publish empty scope");
    rig.deck.inbox.drain();
    let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("live scope");
    assert!(scope.next_due(frame(0), 1).is_none());
}

#[kithara::test]
#[case::slot(0)]
#[case::sequence(1)]
#[case::basis(2)]
#[case::segment(3)]
#[case::load(4)]
fn installed_free_receipt_is_consumed_once(#[case] foreign: u8) {
    let mut rig = rig();
    let (mut track, _inbox) = prepared_track();
    let seq = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    )
    .expect("scheduled start");
    let receipt = answer_next(&mut rig, Ok(()));
    let (outcome, mut batch) = receipt.into();
    let other_seq = rig
        .with_outbox(|out| {
            out.deck(
                When::Next,
                vec![DeckPart::Start {
                    slot: B,
                    fade: Fade::Declick,
                }],
            )
        })
        .expect("live scope")
        .expect("foreign operation admitted")
        .expect("foreign sequence");
    let original_basis = batch.basis.clone();
    let original_segment = track.segment;
    let foreign_seq = if matches!(foreign, 1 | 4) {
        other_seq
    } else {
        seq
    };
    match foreign {
        0 => batch.basis = vec![(B, None)],
        2 => batch.basis = vec![(A, Some(other_seq))],
        3 => track.segment = track.segment.next(),
        _ => {}
    }
    let before = track.snapshot();

    if foreign == 4 {
        let mut first = self::track(B);
        let _ = load(&mut first, &mut rig, Position::ZERO);
        let mut other = self::track(B);
        let foreign_load = load(&mut other, &mut rig, Position::ZERO);
        assert_ne!(foreign_load, seq);
        let _ = rig
            .open(Err(LoadRefusal::Cancelled))
            .expect("first dispatcher reply");
        let opened = opened_fixture(&mut rig, "foreign");
        let receipt = rig
            .open(Ok(opened))
            .expect("independent foreign load reply");
        assert_eq!(receipt.seq(), foreign_load);
        assert!(matches!(
            rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
                .expect("live scope"),
            Settled::Pending
        ));
    }

    assert!(matches!(
        rig.with_outbox(|out| track.settle(
            TrackReceipt::Deck {
                seq: foreign_seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out
        ))
        .expect("live scope"),
        Settled::Pending
    ));
    assert_eq!(track.snapshot().status, before.status);
    assert_eq!(track.snapshot().position, before.position);
    assert_eq!(track.snapshot().speed, before.speed);
    assert_eq!(track.playback_commands.len(), 1);
    batch.basis = original_basis;
    track.segment = original_segment;
    assert!(
        matches!(rig.with_outbox(|out| track.settle(TrackReceipt::Deck {
        seq, outcome: &outcome, batch: &mut batch,
    }, out)).expect("live scope"), Settled::Applied { seq: answered, .. } if answered == seq)
    );
    assert!(track.playback_commands.is_empty());
    assert!(matches!(
        rig.with_outbox(|out| track.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out
        ))
        .expect("live scope"),
        Settled::Pending
    ));
}

#[kithara::test]
fn pause_from_idle_is_noop() {
    let mut rig = rig();
    let mut track = track(A);
    assert_eq!(track.status(), TrackStatus::Idle);

    assert_eq!(
        command_at_zero(&mut track, &mut rig, TrackCommand::Pause { at: When::Next }),
        None
    );

    assert_eq!(
        track.status(),
        TrackStatus::Idle,
        "pause from Idle must not leak a phase transition"
    );
    assert_eq!(track.snapshot().position, Position::ZERO);
    assert!(matches!(
        track.planned(frame(0), mock::SAMPLE_RATE),
        Err(PlayError::Untimed)
    ));
    assert!(track.play.is_none());
    assert!(track.playback_commands.is_empty());
    assert!(rig.block(frame(0), 0.0).is_empty());
    assert_eq!(rig.deck.mixer.held(A), None);
}

#[kithara::test]
fn processor_set_paused_updates_playback() {
    let mut rig = rig();
    let (mut track, _inbox) = prepared_track();
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    );
    let receipt = answer_next(&mut rig, Ok(()));
    answer_track(&mut track, &mut rig, receipt);
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Playing { since: frame(0) }
    );
    assert!(matches!(
        track.snapshot().status,
        TrackStatus::Playing { .. }
    ));
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Pause {
            at: When::At(frame(0)),
        },
    );
    rig.deck.ring.publish().expect("publish Stop");
    rig.deck.inbox.drain();
    {
        let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("live scope");
        let mut due = scope.next_due(frame(0), 1).expect("admitted Stop");
        *due.commands_mut() = vec![DeckPart::Returned(Returned::Stopped {
            slot: A,
            resume: SlotMark {
                session: frame(0),
                lane: LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: 0,
                },
                position: Position::ZERO,
            },
        })];
        due.apply(());
    }
    let kithara_command::ScopedReceipt::Scope(_, receipt) =
        rig.deck.ring.receipt().expect("Stop receipt")
    else {
        panic!("scoped Stop")
    };
    answer_track(&mut track, &mut rig, receipt);
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Paused { at: Position::ZERO }
    );
    assert!(!matches!(
        track.snapshot().status,
        TrackStatus::Playing { .. }
    ));
}

#[kithara::test]
fn player_phase_kind_exhaustive() {
    use kithara_audio::TrackFailureKind;

    let mut rig = rig();
    let mut track = track(A);
    assert_eq!(track.snapshot().status, TrackStatus::Idle);
    load(&mut track, &mut rig, Position::ZERO);
    assert_eq!(track.snapshot().status, TrackStatus::Loading);
    let opened = opened_fixture(&mut rig, "track");
    let receipt = rig.open(Ok(opened)).expect("one opened source");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live scope");
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    assert_eq!(track.snapshot().status, TrackStatus::Loaded);
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    );
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Playing { since: frame(0) }
    );
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Pause {
            at: When::At(frame(0)),
        },
    );
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Paused { at: Position::ZERO }
    );
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    );
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Playing { since: frame(0) }
    );
    let fault = PlaybackFault::Source(TrackFailureKind::SourceCancelled);
    for (event, expected) in [
        (
            DeckEvent::Faded {
                slot: A,
                at: frame(0),
            },
            TrackStatus::Faded { at: frame(0) },
        ),
        (
            DeckEvent::Ended {
                slot: A,
                at: frame(0),
            },
            TrackStatus::Ended { at: frame(0) },
        ),
        (
            DeckEvent::Failed {
                slot: A,
                at: frame(0),
                fault,
            },
            TrackStatus::Failed {
                at: frame(0),
                fault,
            },
        ),
    ] {
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::Play {
                at: When::At(frame(0)),
            },
        );
        let mut receipts = rig.block(frame(0), 0.0);
        settle(&mut track, &mut rig, &mut receipts);
        assert_eq!(track.status(), TrackStatus::Playing { since: frame(0) });
        rig.with_outbox(|out| track.settle(TrackReceipt::Event(event), out))
            .expect("live scope");
        assert_eq!(track.snapshot().status, expected);
    }
    command_at_zero(&mut track, &mut rig, TrackCommand::Release);
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    assert_eq!(track.snapshot().status, TrackStatus::Released);
}

#[kithara::test]
#[case::silent(0)]
#[case::absent_mark(1)]
#[case::foreign_segment(2)]
fn a_relocation_is_refused_off_the_applied_lane(#[case] invalid: u8) {
    let mut rig = rig();
    let (mut track, mut inbox) = prepared_track();
    track.status = if invalid == 0 {
        TrackStatus::Loaded
    } else {
        TrackStatus::Playing { since: frame(0) }
    };
    track.mark = (invalid != 1).then_some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: if invalid == 2 {
                SegmentId::FIRST.next()
            } else {
                SegmentId::FIRST
            },
            frame: 0,
        },
        position: Position::ZERO,
    });
    let before = track.snapshot();
    let output = mock::output(None).get();
    let deck = crate::DeckSnapshot::default();
    let pass = crate::DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: frame(0),
        delivery: FrameCount::new(0),
        output: &output,
        deck: &deck,
    };
    let refused = {
        let mut scope = rig
            .deck
            .ring
            .scope(rig.deck.scope)
            .expect("live deck scope");
        let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher).in_pass(pass);
        track.apply(
            TrackCommand::Jump {
                to: Position::from_secs(1),
                at: frame(96_000),
            },
            &mut out,
        )
    };

    assert!(matches!(refused, Err(PlayError::Untimed)));
    assert!(lane_commands(&mut inbox).is_empty());
    assert_eq!(track.snapshot().position, before.position);
    assert_eq!(track.snapshot().speed, before.speed);
    assert_eq!(track.snapshot().status, before.status);
    assert_eq!(track.snapshot().mark, before.mark);
    assert!(track.lane_commands.is_empty());
    assert!(track.playback_commands.is_empty());
}

#[kithara::test]
fn an_audible_member_under_a_map_the_group_never_applied_is_refused() {
    let mut rig = rig();
    let (mut track, mut inbox) = prepared_track();
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: SegmentId::FIRST.next(),
            frame: 0,
        },
        position: Position::from_secs(1),
    });
    let before = track.snapshot();
    let rate = NonZeroU32::new(48_000).expect("sample rate");

    assert!(matches!(
        track.planned(frame(96_000), rate),
        Err(PlayError::Untimed)
    ));
    assert_eq!(track.snapshot().position, before.position);
    assert_eq!(track.snapshot().speed, before.speed);
    assert_eq!(track.projected(), TrackSettings::default());
    assert_eq!(track.snapshot().mark, before.mark);
    assert_eq!(track.snapshot().status, before.status);
    assert!(lane_commands(&mut inbox).is_empty());
    assert!(track.lane_commands.is_empty());
    assert!(track.playback_commands.is_empty());
    rig.deck.ring.publish().expect("publish empty scope");
    rig.deck.inbox.drain();
    let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("live scope");
    assert!(scope.next_due(frame(0), 1).is_none());
}

#[kithara::test]
fn worker_handoff_geometry_derives_source_output_and_beat_together() {
    let (mut track, mut inbox) = prepared_track();
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: SegmentId::FIRST,
            frame: 0,
        },
        position: Position::ZERO,
    });
    let rate = NonZeroU32::new(48_000).expect("sample rate");
    let mut geometry = Vec::new();
    for (at, source, beat) in [(96_000, 96_000, 4.0), (120_000, 120_000, 5.0)] {
        let (position, speed) = track
            .planned(frame(at), rate)
            .expect("current applied mark");
        let lane = track
            .mark
            .expect("applied mark")
            .lane_at(frame(at))
            .expect("same segment");
        assert_eq!(
            position,
            Position::from_secs_f64(f64::from(source) / 48_000.0)
        );
        assert_eq!(speed, 1.0);
        assert_eq!(
            lane.frame,
            u64::try_from(at).expect("forward session frame")
        );
        assert_eq!(
            session_at(track.mark.expect("applied mark"), lane),
            Some(frame(at))
        );
        assert_eq!(position.as_secs_f64() * 2.0, beat);
        geometry.push((position, frame(at), beat));
    }
    assert_ne!(
        geometry[0], geometry[1],
        "a later boundary cannot reuse an earlier projection"
    );
    assert_eq!(track.snapshot().position, Position::ZERO);
    assert!(lane_commands(&mut inbox).is_empty());
    assert!(track.lane_commands.is_empty());
    assert!(track.playback_commands.is_empty());
}

#[kithara::test]
#[case::absent_owner(0)]
#[case::closed_dispatcher(1)]
#[case::unready_geometry(2)]
fn a_staged_preparation_needs_an_owner_and_a_stageable_load(#[case] refusal: u8) {
    let mut rig = rig();
    let mut track = track(A);
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
        ResourceSrc::parse("https://example.com/song.mp3").expect("original source"),
    )
    .store(AssetStore::builder(pools()).build())
    .maybe_worker(
        (refusal != 0)
            .then(|| crate::PlayWorker::new(crate::PlayWorkerConfig::builder(pools()).build())),
    )
    .host_sample_rate(mock::SAMPLE_RATE)
    .audio_buffer_chunks(if refusal == 2 {
        std::num::NonZeroUsize::MAX
    } else {
        std::num::NonZeroUsize::new(3).expect("ring depth")
    })
    .build();
    let load = ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()));
    if refusal == 1 {
        let (_, replacement) = channel(ChannelConfig::builder().build());
        drop(std::mem::replace(&mut rig.deck.opens, replacement));
    }
    let before = track.snapshot();
    let refused = rig
        .with_outbox(|out| {
            track.apply(
                TrackCommand::Load {
                    item: load,
                    position: Position::ZERO,
                },
                out,
            )
        })
        .expect("live deck scope");

    assert!(refused.is_err());
    assert_eq!(track.snapshot().status, before.status);
    assert_eq!(track.snapshot().position, before.position);
    assert_eq!(track.snapshot().speed, before.speed);
    assert_eq!(track.snapshot().mark, before.mark);
    assert!(track.loading.is_none());
    assert!(track.lane.is_none());
    rig.deck.opens.drain();
    assert!(rig.deck.opens.next_due((), 1).is_none());
    rig.deck.ring.publish().expect("publish empty scope");
    rig.deck.inbox.drain();
    let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("live scope");
    assert!(scope.next_due(frame(0), 1).is_none());
}

#[kithara::test]
fn playback_shared_seek_epoch_increments() {
    let mut rig = rig();
    let mut track = track(A);
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
        ResourceSrc::parse("https://example.com/song.mp3").expect("original source"),
    )
    .store(AssetStore::builder(pools()).build())
    .worker(crate::PlayWorker::new(
        crate::PlayWorkerConfig::builder(pools()).build(),
    ))
    .host_sample_rate(mock::SAMPLE_RATE)
    .build();
    let item = ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()));
    rig.with_outbox(|out| {
        track.apply(
            TrackCommand::Load {
                item,
                position: Position::ZERO,
            },
            out,
        )
    })
    .expect("live deck scope")
    .expect("load admitted");
    assert_eq!(track.segment, SegmentId::FIRST);
    for expected in [1, 2, 3] {
        rig.with_outbox(|out| track.apply(TrackCommand::Seek { to: Position::ZERO }, out))
            .expect("live deck scope")
            .expect("seek admitted");
        assert_eq!(track.segment.get(), expected);
    }
    rig.deck.opens.drain();
    let mut request = rig.deck.opens.next_due((), 1).expect("one original load");
    let DispatcherCommand::Load(load) = request.commands_mut().pop().expect("load command") else {
        panic!("unexpected dispatcher command");
    };
    let mut inbox = load.inbox;
    let commands = lane_commands(&mut inbox);
    assert_eq!(commands.len(), 3);
    for (command, expected) in commands.iter().zip([1, 2, 3]) {
        assert!(matches!(command, LaneCommand::Segment { id, from, .. }
            if id.get() == expected && *from == Position::ZERO));
    }
}

#[kithara::test]
fn withdrawing_the_newest_epoch_unpublishes_the_seek() {
    let mut rig = rig();
    let (sender, mut inbox) = lane();
    let mut track = track(A);
    track.lane = Some(sender);
    track.status = TrackStatus::Loaded;
    track.attach_at = Some(frame(0));
    let before = track.snapshot();
    rig.with_outbox(|out| {
        track.apply(
            TrackCommand::Seek {
                to: Position::from_secs(8),
            },
            out,
        )
    })
    .expect("live deck scope")
    .expect("seek admitted");
    assert_eq!(track.snapshot().position, before.position);
    let refused = answer_next(&mut rig, Err(DeckRefusal::Outdated { slot: A }));
    assert!(matches!(
        answer_track(&mut track, &mut rig, refused),
        Settled::Pending
    ));
    assert_eq!(track.snapshot().position, before.position);
    let retry = answer_next(&mut rig, Ok(()));
    assert!(
        matches!(retry.batch().commands.as_slice(), [DeckPart::Adopt { slot, segment }]
        if *slot == A && *segment == SegmentId::FIRST.next())
    );
    assert!(matches!(
        answer_track(&mut track, &mut rig, retry),
        Settled::Applied { .. }
    ));
    rig.with_outbox(|out| {
        track.apply(
            TrackCommand::Seek {
                to: Position::from_secs(8),
            },
            out,
        )
    })
    .expect("live deck scope")
    .expect("fresh seek admitted");
    assert_eq!(track.segment.get(), 2);
    assert_eq!(track.snapshot().position, before.position);
    assert!(matches!(lane_commands(&mut inbox).as_slice(),
        [LaneCommand::Segment { id: first, from: first_from, .. },
         LaneCommand::Segment { id: second, from: second_from, .. }]
        if first.get() == 1 && second.get() == 2
            && *first_from == Position::from_secs(8) && *second_from == Position::from_secs(8)));
    let receipt = answer_next(&mut rig, Ok(()));
    assert!(
        matches!(receipt.batch().commands.as_slice(), [DeckPart::Adopt { slot, segment }]
        if *slot == A && segment.get() == 2)
    );
    let output = mock::output(None).get();
    let deck = crate::DeckSnapshot {
        slots: vec![SlotSnapshot {
            position: 8.0,
            mark: Some(SlotMark {
                session: frame(0),
                lane: LaneFrame {
                    segment: track.segment,
                    frame: 0,
                },
                position: Position::from_secs(8),
            }),
            ..SlotSnapshot::default()
        }],
        ..crate::DeckSnapshot::default()
    };
    let pass = crate::DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: frame(0),
        delivery: FrameCount::new(0),
        output: &output,
        deck: &deck,
    };
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    let mut scope = rig.deck.ring.scope(rig.deck.scope).expect("live scope");
    let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher).in_pass(pass);
    assert!(
        matches!(track.settle(TrackReceipt::Deck { seq, outcome: &outcome, batch: &mut batch },
        &mut out), Settled::Applied { seq: answered, .. } if answered == seq)
    );
    assert_eq!(track.snapshot().position, Position::from_secs(8));
    assert!(track.adopting.is_empty());
}

#[kithara::test]
fn withdrawing_an_overtaken_epoch_leaves_the_newer_seek_published() {
    let mut rig = rig();
    let (sender, _inbox) = lane();
    let mut track = track(A);
    track.lane = Some(sender);
    track.status = TrackStatus::Loaded;
    track.attach_at = Some(frame(0));
    for seconds in [1, 2] {
        rig.with_outbox(|out| {
            track.apply(
                TrackCommand::Seek {
                    to: Position::from_secs(seconds),
                },
                out,
            )
        })
        .expect("live deck scope")
        .expect("seek admitted");
    }
    let newest = track.segment;
    assert_eq!(newest.get(), 2);
    let overtaken = answer_next(&mut rig, Err(DeckRefusal::Outdated { slot: A }));
    assert!(matches!(
        answer_track(&mut track, &mut rig, overtaken),
        Settled::Pending
    ));
    assert_eq!(track.segment, newest);
    assert_eq!(track.snapshot().position, Position::ZERO);
    let superseded = answer_next(&mut rig, Ok(()));
    assert!(matches!(
        superseded.outcome(),
        Outcome::Rejected(Rejection::Stale)
    ));
    assert!(matches!(
        answer_track(&mut track, &mut rig, superseded),
        Settled::Pending
    ));
    for _ in 0..2 {
        let receipt = answer_next(&mut rig, Ok(()));
        assert!(
            matches!(receipt.batch().commands.as_slice(), [DeckPart::Adopt { segment, .. }]
            if *segment == newest)
        );
        assert!(matches!(
            answer_track(&mut track, &mut rig, receipt),
            Settled::Applied { .. }
        ));
    }
    assert_eq!(track.segment, newest);
    assert!(track.adopting.is_empty());
}

#[kithara::test]
fn an_epoch_adopted_before_it_is_published_reports_the_audio_thread() {
    let mut rig = rig();
    let (sender, _inbox) = lane();
    let mut track = track(A);
    track.lane = Some(sender);
    track.status = TrackStatus::Loaded;
    track.attach_at = Some(frame(0));
    let output = mock::output(None).get();
    let deck = crate::DeckSnapshot {
        slots: vec![SlotSnapshot {
            position: 1.5,
            duration: 162.0,
            ..SlotSnapshot::default()
        }],
        ..crate::DeckSnapshot::default()
    };
    let pass = crate::DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: frame(0),
        delivery: FrameCount::new(0),
        output: &output,
        deck: &deck,
    };
    {
        let mut scope = rig
            .deck
            .ring
            .scope(rig.deck.scope)
            .expect("live deck scope");
        let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher).in_pass(pass);
        track
            .apply(TrackCommand::Seek { to: Position::ZERO }, &mut out)
            .expect("seek admitted");
    }
    let receipt = answer_next(&mut rig, Ok(()));
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    {
        let mut scope = rig
            .deck
            .ring
            .scope(rig.deck.scope)
            .expect("live deck scope");
        let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher).in_pass(pass);
        track.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            &mut out,
        );
    }
    let during_send = track.snapshot();
    assert!((during_send.position.as_secs_f64() - 1.5).abs() < f64::EPSILON);
    assert!(
        (during_send.duration.expect("known duration").as_secs_f64() - 162.0).abs() < f64::EPSILON
    );
}

/// What the dispatcher opens for a track of `src`.
pub(super) fn opened_fixture(rig: &mut Rig, src: &str) -> Loaded<OpenedTrack> {
    let dir = TestTempDir::new();
    let path = dir.path().join("track.wav");
    let spec = AudioSpec::new(2, mock::SAMPLE_RATE);
    let samples = vec![
        kithara_audio::mock::TEST_PCM_DEFAULT_VALUE;
        usize::try_from(mock::SAMPLE_RATE.get()).expect("sample rate") / 100 * 2
    ];
    mock::write_pcm_wav(&path, &samples, spec).expect("generated float WAV");
    let worker = crate::PlayWorker::new(crate::PlayWorkerConfig::builder(pools()).build());
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(ResourceSrc::Path(path))
        .store(AssetStore::builder(pools()).build())
        .worker(worker.clone())
        .host_sample_rate(mock::SAMPLE_RATE)
        .build();
    let (_, config) = mock::resource_tracks(&config).expect("local source configuration");
    let futures::future::Either::Left(config) = config else {
        panic!("WAV is a file source")
    };
    let load = mock::track_load(
        config,
        Arc::from(src),
        worker,
        mock::SAMPLE_RATE,
        |worker, config, position, start, inbox| {
            Box::pin(async move { worker.load(config, position, start, inbox).await })
        },
    );
    let opened = rig
        .runtime
        .block_on(rig.deck.load_fixture(load, Position::ZERO))
        .expect("real dispatcher opens the WAV once");
    rig.sources.push(dir);
    opened
}

pub(super) fn open_at_position(
    rig: &mut Rig,
    opened: Loaded<OpenedTrack>,
) -> (
    Receipt<DispatcherProtocol<ResourceLoad<TestPools>>>,
    Position,
) {
    rig.opens.drain();
    let due = rig.opens.next_due((), 1).expect("one open");
    let [DispatcherCommand::Load(request)] = due.commands() else {
        panic!("expected a dispatcher load");
    };
    let position = request.position;
    due.apply(Dispatched::Loaded(opened));
    let receipt = rig.dispatcher.receipts().next().expect("one load receipt");
    (receipt, position)
}

/// The commands the lane takes in its next block, in order.
pub(super) fn lane_commands(inbox: &mut Inbox<LaneProtocol>) -> Vec<LaneCommand> {
    inbox.drain();
    let mut commands = Vec::new();
    while let Some(due) = inbox.next_due(
        LaneFrame {
            segment: SegmentId::FIRST,
            frame: 0,
        },
        1,
    ) {
        commands.extend(due.commands().iter().cloned());
        due.apply(kithara_render::LaneApplied {
            engine_latency: FrameCount::new(0),
            ready: None,
        });
    }
    commands
}

/// Hands every receipt to `track`, in order, and returns what it reported.
pub(super) fn settle(track: &mut Track, rig: &mut Rig, receipts: &mut [Answer]) -> Vec<Settled> {
    receipts
        .iter_mut()
        .map(|receipt| {
            rig.with_outbox(|out| {
                track.settle(
                    TrackReceipt::Deck {
                        seq: receipt.seq,
                        outcome: &receipt.outcome,
                        batch: &mut receipt.batch,
                    },
                    out,
                )
            })
            .expect("live deck scope")
        })
        .collect()
}

/// Asks the dispatcher to open a track that stands at `position` once attached.
pub(super) fn load(track: &mut Track, rig: &mut Rig, position: Position) -> Seq {
    rig.with_outbox(|out| {
        track.apply(
            TrackCommand::Load {
                item: item(),
                position,
            },
            out,
        )
    })
    .expect("live deck scope")
    .expect("the dispatcher has room")
    .expect("a load goes out on its own")
}

/// A track of `src` in `slot`, its consumer attached on frame 0.
pub(super) fn loaded(rig: &mut Rig, slot: Slot, src: &str) -> (Track, Inbox<LaneProtocol>) {
    let (sender, inbox) = lane();
    let mut track = track(slot);
    load(&mut track, rig, Position::ZERO);
    let opened = opened_fixture(rig, src);
    let receipt = rig
        .open(Ok(opened))
        .expect("the load reached the dispatcher");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope");
    track.lane = Some(sender);
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, rig, &mut receipts);
    assert_eq!(track.snapshot().status, TrackStatus::Loaded);
    (track, inbox)
}

#[kithara::test]
fn a_load_opens_its_source_once_and_attaches_on_the_next_block() {
    let mut rig = rig();
    let mut track = track(A);

    let seq = load(&mut track, &mut rig, Position::ZERO);
    assert_eq!(track.snapshot().status, TrackStatus::Loading);
    let opened = opened_fixture(&mut rig, "track");
    let receipt = rig
        .open(Ok(opened))
        .expect("the load reached the dispatcher");
    let unopened = opened_fixture(&mut rig, "track");
    assert!(rig.open(Ok(unopened)).is_none(), "one open per load");
    assert!(matches!(
        rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
            .expect("live deck scope"),
        Settled::Pending
    ));
    assert_eq!(
        rig.deck.mixer.held(A),
        None,
        "the attach waits for the block"
    );

    let mut receipts = rig.block(frame(0), 0.0);
    let settled = settle(&mut track, &mut rig, &mut receipts);

    assert!(
        matches!(settled.as_slice(), [Settled::Applied { seq: answered, .. }] if *answered == seq),
        "the attach answers the load: {settled:?}"
    );
    assert_eq!(rig.deck.mixer.held(A), Some("track"));
    assert_eq!(track.snapshot().status, TrackStatus::Loaded);
}

#[kithara::test]
#[case(0.0)]
#[case(-1.0)]
fn a_cued_subfloor_speed_reaches_the_lane_at_the_floor(#[case] speed: f32) {
    use crate::player::Track as _;

    let mut rig = rig();
    let (sender, mut inbox) = lane();
    let mut track = track(A);
    track.lane = Some(sender);
    rig.with_outbox(|out| track.cue(Position::ZERO, speed, out))
        .expect("live deck scope")
        .expect("finite cue speed is admitted");

    assert!(matches!(
        lane_commands(&mut inbox).as_slice(),
        [LaneCommand::Segment { speed: SpeedCurve::Constant(sent), .. }]
            if *sent == kithara_warp::MIN_SPEED
    ));
    assert_eq!(track.segment_speed, kithara_warp::MIN_SPEED);
    assert_eq!(track.projected().speed(), kithara_warp::MIN_SPEED);
}

#[kithara::test]
#[case::ramp(SpeedCurve::Ramp { to: 0.0, frames: std::num::NonZeroU64::MIN })]
#[case::steps(SpeedCurve::Steps(Arc::from([(0, 0.0), (1, -1.0)])))]
fn a_speed_curve_clamps_every_finite_subfloor_point(#[case] speed: SpeedCurve) {
    use crate::player::Track as _;

    let mut rig = rig();
    let (sender, mut inbox) = lane();
    let mut track = track(A);
    track.lane = Some(sender);
    rig.with_outbox(|out| {
        track.apply(
            TrackCommand::SetSpeed {
                speed,
                at: When::Next,
            },
            out,
        )
    })
    .expect("live deck scope")
    .expect("finite curve speeds are admitted");

    let commands = lane_commands(&mut inbox);
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        LaneCommand::SetSpeed(SpeedCurve::Ramp { to, frames }) => {
            assert_eq!(*to, kithara_warp::MIN_SPEED);
            assert_eq!(*frames, std::num::NonZeroU64::MIN);
        }
        LaneCommand::SetSpeed(SpeedCurve::Steps(steps)) => {
            assert_eq!(
                steps.as_ref(),
                [(0, kithara_warp::MIN_SPEED), (1, kithara_warp::MIN_SPEED)]
            );
        }
        command => panic!("unexpected lane command: {command:?}"),
    }
    assert_eq!(track.projected().speed(), kithara_warp::MIN_SPEED);
}

#[kithara::test]
fn a_track_loaded_at_a_position_stands_there_once_attached() {
    let mut rig = rig();
    let mut track = track(A);
    let seq = load(&mut track, &mut rig, Position::from_secs(3));
    let opened = opened_fixture(&mut rig, "track");
    let (receipt, position) = open_at_position(&mut rig, opened);
    assert_eq!(position, Position::from_secs(3));
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope");

    let mut receipts = rig.block(frame(0), 0.0);
    assert_eq!(receipts.len(), 1);
    let settled = settle(&mut track, &mut rig, &mut receipts);

    assert!(
        matches!(settled.as_slice(), [Settled::Applied { seq: answered, .. }] if *answered == seq),
        "the attach answers the load: {settled:?}"
    );
    assert!(receipts[0].batch().commands.is_empty());
    assert_eq!(rig.deck.mixer.held(A), Some("track"));
    assert_eq!(track.snapshot().position, Position::from_secs(3));
}

#[kithara::test]
fn a_refused_open_leaves_the_track_idle() {
    let mut rig = rig();
    let mut track = track(A);
    let seq = load(&mut track, &mut rig, Position::ZERO);

    let receipt = rig
        .open(Err(LoadRefusal::Cancelled))
        .expect("the load reached the dispatcher");
    let settled = rig
        .with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope");

    assert!(
        matches!(settled, Settled::Rejected { seq: answered, .. } if answered == seq),
        "{settled:?}"
    );
    assert_eq!(track.snapshot().status, TrackStatus::Idle);
    assert!(
        rig.block(frame(0), 0.0).is_empty(),
        "nothing went to the deck"
    );
}

#[kithara::test]
fn play_sounds_from_its_frame_and_pause_reports_where_it_stopped() {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "track");

    let play = in_pass(&mut rig, frame(0), |out| {
        track.apply(
            TrackCommand::Play {
                at: When::At(frame(4_096)),
            },
            out,
        )
    })
    .expect("the deck has room")
    .expect("a play goes out on its own");
    assert!(
        rig.block(frame(0), 0.0).is_empty(),
        "the start waits for its frame"
    );
    let mut receipts = rig.block(frame(4_096), 0.0);
    let settled = settle(&mut track, &mut rig, &mut receipts);
    assert!(
        matches!(settled.as_slice(), [Settled::Applied { seq, at }] if *seq == play && *at == frame(4_096)),
        "{settled:?}"
    );
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Playing {
            since: frame(4_096)
        }
    );

    in_pass(&mut rig, frame(4_096), |out| {
        track.apply(
            TrackCommand::Pause {
                at: When::At(frame(8_192)),
            },
            out,
        )
    })
    .expect("the deck has room");
    let mut receipts = rig.block(frame(8_192), 1.5);
    settle(&mut track, &mut rig, &mut receipts);

    let snapshot = track.snapshot();
    assert_eq!(
        snapshot.status,
        TrackStatus::Paused {
            at: Position::from_secs_f64(1.5)
        }
    );
    assert_eq!(snapshot.position, Position::from_secs_f64(1.5));
}

#[kithara::test]
fn a_track_played_after_another_chains_from_its_slot() {
    let mut rig = rig();
    let (mut leading, _) = loaded(&mut rig, A, "leading");
    let (mut following, _) = loaded(&mut rig, B, "following");

    command_at_zero(
        &mut leading,
        &mut rig,
        TrackCommand::Play { at: When::Next },
    );
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut leading, &mut rig, &mut receipts);
    rig.with_outbox(|out| following.apply(TrackCommand::PlayAfter { track: A }, out))
        .expect("live deck scope")
        .expect("the deck has room");
    assert!(
        rig.block(frame(0), 0.0).is_empty(),
        "the chain waits for the leading track's end"
    );
    let mut receipts = rig.end(A, frame(4_096));

    assert!(
        matches!(
            receipts.as_slice(),
            [receipt] if matches!(
                receipt.batch().commands.as_slice(),
                [DeckPart::Chain { from, to }] if *from == A && *to == B
            ) && receipt.batch().basis.iter().map(|&(slot, _)| slot).eq([A, B])
        ),
        "{:?}",
        receipts
            .iter()
            .map(|receipt| &receipt.batch().commands)
            .collect::<Vec<_>>()
    );
    assert!(matches!(receipts[0].outcome, Outcome::Applied { at, .. } if at == frame(4_096)));
    settle(&mut leading, &mut rig, &mut receipts);
    settle(&mut following, &mut rig, &mut receipts);
    rig.with_outbox(|out| {
        leading.settle(
            TrackReceipt::Event(DeckEvent::Ended {
                slot: A,
                at: frame(4_096),
            }),
            out,
        )
    })
    .expect("live deck scope");
    assert_eq!(
        leading.snapshot().status,
        TrackStatus::Ended { at: frame(4_096) }
    );
    assert_eq!(
        following.snapshot().status,
        TrackStatus::Playing {
            since: frame(4_096)
        }
    );
}

#[kithara::test]
fn every_part_a_track_sends_names_its_slot() {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, B, "track");
    for command in [
        TrackCommand::Fade {
            at: When::Next,
            settings: CrossfadeSettings::default(),
            dir: FadeDir::In,
        },
        TrackCommand::Seek {
            to: Position::from_secs(1),
        },
        TrackCommand::Release,
    ] {
        rig.with_outbox(|out| track.apply(command, out))
            .expect("live deck scope")
            .expect("the deck has room");
    }

    let mut receipts = rig.block(frame(0), 0.0);
    let parts: Vec<_> = receipts
        .iter()
        .flat_map(|receipt| receipt.batch().commands.iter())
        .collect();
    assert!(
        matches!(
            parts.as_slice(),
            [
                DeckPart::Start { slot: started, fade: Fade::Crossfade(_) },
                DeckPart::Adopt { slot: sought, .. },
                DeckPart::Returned(Returned::Pcm { slot: released, .. }),
            ] if [*started, *sought, *released] == [B; 3]
        ),
        "{parts:?}"
    );
    assert!(receipts.iter_mut().all(|receipt| {
        TrackReceipt::<TestPools>::Deck {
            seq: receipt.seq,
            outcome: &receipt.outcome,
            batch: &mut receipt.batch,
        }
        .names(B)
    }));
}

#[kithara::test]
fn a_release_frees_the_track_once_the_mixer_let_its_consumer_go() {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "track");

    rig.with_outbox(|out| track.apply(TrackCommand::Release, out))
        .expect("live deck scope")
        .expect("the deck has room");
    assert_ne!(
        track.snapshot().status,
        TrackStatus::Released,
        "the detach waits for its receipt"
    );
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);

    assert_eq!(track.snapshot().status, TrackStatus::Released);
    assert_eq!(rig.deck.mixer.held(A), None);
}

#[kithara::test]
fn detaching_a_member_withdraws_its_preparation() {
    let mut rig = rig();
    let (mut track, _inbox) = loaded(&mut rig, A, "track");
    track.ready = None;
    assert_eq!(
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::Play {
                at: When::At(frame(64)),
            }
        ),
        None
    );
    assert_eq!(track.play, Some(When::At(frame(64))));
    command_at_zero(&mut track, &mut rig, TrackCommand::Release);
    let mut receipts = rig.block(frame(0), 0.0);
    assert_eq!(receipts.len(), 1);
    assert!(matches!(
        settle(&mut track, &mut rig, &mut receipts).as_slice(),
        [Settled::Applied { .. }]
    ));
    assert_eq!(track.status(), TrackStatus::Released);
    assert!(track.play.is_none());
    assert!(track.ready.is_none());
    assert!(track.playback_commands.is_empty());
    assert_eq!(rig.deck.mixer.held(A), None);
    assert!(matches!(
        settle(&mut track, &mut rig, &mut receipts).as_slice(),
        [Settled::Pending]
    ));
    rig.with_outbox(|out| track.tick(frame(64), out))
        .expect("live scope");
    assert!(rig.block(frame(64), 0.0).is_empty());
    assert_eq!(track.status(), TrackStatus::Released);
}

#[kithara::test]
fn an_evicting_track_takes_the_slot_over_on_its_frame() {
    let mut rig = rig();
    let (mut old, _) = loaded(&mut rig, A, "old");
    rig.with_outbox(|out| old.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("live deck scope")
        .expect("the deck has room");
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut old, &mut rig, &mut receipts);

    let mut new = unseated_track();
    load(&mut new, &mut rig, Position::ZERO);

    let opened = opened_fixture(&mut rig, "new");
    let receipt = rig.open(Ok(opened)).expect("one open");
    rig.with_outbox(|out| new.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope");
    assert_eq!(rig.deck.mixer.held(A), Some("old"));
    let output = mock::output(None).get();
    let deck = crate::DeckSnapshot {
        slots: vec![SlotSnapshot {
            state: SlotState::Playing,
            ..Default::default()
        }],
        ..Default::default()
    };
    let pass = crate::DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: frame(0),
        delivery: FrameCount::new(0),
        output: &output,
        deck: &deck,
    };
    let seq = {
        let mut scope = rig
            .deck
            .ring
            .scope(rig.deck.scope)
            .expect("live deck scope");
        let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher).in_pass(pass);
        let at = When::At(frame(1_024));
        let result = out.together_owned(at, |out| {
            new.apply(TrackCommand::Seat { slot: A, at }, out)?;
            new.apply(
                TrackCommand::Fade {
                    at,
                    settings: CrossfadeSettings::default(),
                    dir: FadeDir::In,
                },
                out,
            )?;
            Ok(())
        });
        match result {
            Ok(((), Some(seq))) => {
                new.finish_group(Ok(seq));
                seq
            }
            Ok(((), None)) => panic!("the replacement sends one group"),
            Err((error, mut parts)) => {
                new.finish_group(Err(&mut parts));
                panic!("the replacement group was refused: {error}");
            }
        }
    };
    assert_eq!(
        rig.deck.mixer.held(A),
        Some("old"),
        "the replace waits for its frame"
    );

    let mut receipts = rig.block(frame(1_024), 0.0);
    settle(&mut old, &mut rig, &mut receipts);
    let settled = settle(&mut new, &mut rig, &mut receipts);

    assert!(
        matches!(
            receipts[0].batch().commands.as_slice(),
            [
                DeckPart::Returned(Returned::Pcm { slot, pcm }),
                DeckPart::Start { slot: started, fade: Fade::Crossfade(_) },
            ] if *slot == A && &**pcm.src() == "old" && *started == A
        ),
        "{:?}",
        receipts[0].batch().commands
    );
    assert!(
        matches!(settled.as_slice(), [Settled::Applied { seq: answered, at }] if *answered == seq && *at == frame(1_024)),
        "the replace answers the group: {settled:?}"
    );
    assert_eq!(rig.deck.mixer.held(A), Some("new"));
    assert_eq!(old.snapshot().status, TrackStatus::Released);
    assert_eq!(
        new.snapshot().status,
        TrackStatus::Playing {
            since: frame(1_024)
        }
    );
}

#[kithara::test]
#[case::released_replacement_before_its_frame(false, false)]
#[case::released_replacement_the_deck_already_applied(true, false)]
#[case::parked_repeat(false, true)]
fn superseding_a_scheduled_batch_leaves_the_slot_sounding_as_it_was(
    #[case] applied: bool,
    #[case] repeat: bool,
) {
    let mut rig = rig();
    let (mut old, mut lane) = loaded(&mut rig, A, "old");
    rig.with_outbox(|out| old.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("deck scope")
        .expect("start");
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut old, &mut rig, &mut receipts);
    if repeat {
        let original = old.segment;
        let seq = rig
            .with_outbox(|out| old.apply(TrackCommand::PlayAfter { track: A }, out))
            .expect("deck scope")
            .expect("repeat")
            .expect("parked batch");
        rig.with_outbox(|out| old.apply(TrackCommand::Supersede, out))
            .expect("deck scope")
            .expect("supersede");
        assert!(
            matches!(
                rig.with_outbox(|out| old.apply(TrackCommand::Seek { to: Position::ZERO }, out,))
                    .expect("deck scope"),
                Err(PlayError::NotReady)
            ),
            "a pending supersession retains the lane credit needed to restore its segment"
        );
        let mut receipts = rig.block(frame(0), 0.0);
        assert!(receipts.iter().any(|receipt| receipt.seq == seq
            && matches!(receipt.outcome, Outcome::Rejected(Rejection::Stale))));
        assert!(
            settle(&mut old, &mut rig, &mut receipts)
                .iter()
                .all(|settled| matches!(settled, Settled::Pending))
        );
        assert_eq!(old.segment, original);
        lane.drain();
        let mut restored = false;
        while let Some(due) = lane.next_due(LaneFrame::default(), 1) {
            restored |= due.commands().iter().any(
                |command| matches!(command, LaneCommand::Segment { id, .. } if *id == original),
            );
            due.apply(kithara_render::LaneApplied {
                engine_latency: FrameCount::new(0),
                ready: Some(original),
            });
        }
        assert!(restored, "supersession restores the lane's segment too");
        rig.with_outbox(|out| old.tick(frame(64), out))
            .expect("deck scope");
        let mut receipts = rig.block(frame(64), 0.0);
        assert!(!receipts.iter().any(|receipt| {
            receipt
                .batch
                .commands
                .iter()
                .any(|part| matches!(part, DeckPart::Adopt { .. }))
        }));
        settle(&mut old, &mut rig, &mut receipts);
        assert_eq!(rig.mixer.held(A), Some("old"));
        assert!(matches!(old.snapshot().status, TrackStatus::Playing { .. }));
        let mut receipts = rig.end(A, frame(1_024));
        assert!(!receipts.iter().any(|receipt| {
            receipt
                .batch
                .commands
                .iter()
                .any(|part| matches!(part, DeckPart::Adopt { .. }))
        }));
        settle(&mut old, &mut rig, &mut receipts);
        rig.mixer
            .report(DeckEvent::Ended {
                slot: A,
                at: frame(1_024),
            })
            .expect("event ring");
        let event = rig.events.drain().next().expect("Ended event");
        assert!(matches!(event, DeckEvent::Ended { slot: A, .. }));
        rig.with_outbox(|out| old.settle(TrackReceipt::Event(event), out))
            .expect("deck scope");
        assert!(matches!(old.snapshot().status, TrackStatus::Ended { .. }));
        rig.with_outbox(|out| {
            old.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(1.25), When::Next),
                out,
            )
        })
        .expect("deck scope")
        .expect("configuration after cancellation is ready and timely");
        rig.runtime.block_on(async {
            a_cancelled_repeat_reaches_its_original_end(441).await;
            a_cancelled_repeat_reaches_its_original_end(66_150).await;
        });
        return;
    }
    let mut new = unseated_track();
    load(&mut new, &mut rig, Position::ZERO);

    let opened = opened_fixture(&mut rig, "new");
    let receipt = rig.open(Ok(opened)).expect("replacement load");
    rig.with_outbox(|out| new.settle(TrackReceipt::Loaded(receipt), out))
        .expect("deck scope");
    let output = mock::output(None).get();
    let deck = crate::DeckSnapshot {
        slots: vec![SlotSnapshot {
            state: SlotState::Playing,
            ..Default::default()
        }],
        ..Default::default()
    };
    let seq = {
        let mut scope = rig.deck.ring.scope(rig.deck.scope).expect("deck scope");
        let mut out = Outbox::new(&mut scope, &mut rig.deck.dispatcher).in_pass(crate::DeckPass {
            mix: DeckMixSettings::default(),
            suspended: false,
            now: frame(0),
            delivery: FrameCount::new(0),
            output: &output,
            deck: &deck,
        });
        let at = When::At(frame(1_024));
        let (_, seq) = out
            .together(at, |out| {
                new.apply(TrackCommand::Seat { slot: A, at }, out)?;
                new.apply(
                    TrackCommand::Fade {
                        at,
                        settings: CrossfadeSettings::default(),
                        dir: FadeDir::In,
                    },
                    out,
                )?;
                Ok(())
            })
            .expect("replacement group");
        let seq = seq.expect("group sequence");
        new.finish_group(Ok(seq));
        seq
    };
    let mut receipts = if applied {
        rig.block(frame(1_024), 0.0)
    } else {
        Vec::new()
    };
    rig.with_outbox(|out| new.apply(TrackCommand::Release, out))
        .expect("deck scope")
        .expect("release replacement");
    receipts.extend(rig.block(frame(if applied { 1_088 } else { 64 }), 0.0));
    assert!(!receipts.iter().any(|receipt| {
        receipt
            .batch
            .commands
            .iter()
            .any(|part| matches!(part, DeckPart::Detach { .. }))
    }));
    let settled = settle(&mut new, &mut rig, &mut receipts);
    if applied {
        assert!(settled.iter().any(
            |settled| matches!(settled, Settled::Applied { seq: answer, .. } if *answer == seq)
        ));
        rig.with_outbox(|out| new.tick(frame(1_152), out))
            .expect("deck scope");
        let mut receipts = rig.block(frame(1_152), 0.0);
        settle(&mut new, &mut rig, &mut receipts);
        assert_eq!(rig.mixer.held(A), None);
    } else {
        assert!(settled.iter().any(|settled| matches!(settled,
            Settled::Rejected { seq: answer, reason: Rejection::Stale } if *answer == seq)));
        rig.block(frame(1_024), 0.0);
        assert_eq!(rig.mixer.held(A), Some("old"));
        assert!(matches!(old.snapshot().status, TrackStatus::Playing { .. }));
    }
    assert_eq!(new.snapshot().status, TrackStatus::Released);
}

pub(super) fn with_mixer<R>(
    mixer: &mut mock::MixerRig,
    dispatcher: &mut Sender<DispatcherProtocol<ResourceLoad<TestPools>>>,
    now: SessionFrame,
    run: impl FnOnce(&mut Outbox<'_, TestPools>) -> R,
) -> R {
    let output = mock::output(None).get();
    let deck = mixer.ends.snapshot.read();
    let mut scope = mixer.ring.scope(mixer.scope).expect("live mixer scope");
    let mut out = Outbox::new(&mut scope, dispatcher).in_pass(crate::DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now,
        delivery: FrameCount::new(0),
        output: &output,
        deck,
    });
    run(&mut out)
}

/// A real WAV lane of `frames` stereo frames at the deck rate, opened through the dispatcher.
fn repeat_wav_load(
    dir: &TestTempDir,
    frames: usize,
) -> (crate::PlayWorker<TestPools>, ResourceLoad<TestPools>) {
    let path = dir.path().join("repeat.wav");
    mock::write_pcm_wav(
        &path,
        &vec![0.5; frames * 2],
        AudioSpec::new(2, mock::SAMPLE_RATE),
    )
    .expect("float WAV at the deck rate");
    let worker = crate::PlayWorker::new(crate::PlayWorkerConfig::builder(pools()).build());
    let config = ResourceConfig::for_src(ResourceSrc::Path(path))
        .store(AssetStore::builder(pools()).build())
        .worker(worker.clone())
        .host_sample_rate(mock::SAMPLE_RATE)
        .build();
    let (_, config) = mock::resource_tracks(&config).expect("file config");
    let futures::future::Either::Left(config) = config else {
        panic!("WAV source")
    };
    let item = mock::track_load(
        config,
        Arc::from("repeat"),
        worker.clone(),
        mock::SAMPLE_RATE,
        |worker, config, position, start, inbox| {
            Box::pin(async move { worker.load(config, position, start, inbox).await })
        },
    );
    (worker, item)
}

pub(super) async fn a_cancelled_repeat_reaches_its_original_end(frames: usize) {
    use kithara_render::rt::StreamShape;

    let dir = TestTempDir::new();
    let (worker, item) = repeat_wav_load(&dir, frames);
    let (mut dispatcher, inbox) = channel(ChannelConfig::builder().build());
    let driver = worker
        .start_dispatcher(inbox)
        .expect("the worker owns its dispatcher");
    let shape = StreamShape::new(
        NonZeroU32::new(64).expect("block frames"),
        mock::SAMPLE_RATE,
    );
    let mut mixer =
        mock::MixerRig::new(DeckMixerConfig::default(), shape, &pools()).expect("real mixer");
    let mut track = track(A);
    with_mixer(&mut mixer, &mut dispatcher, frame(0), |out| {
        track.apply(
            TrackCommand::Load {
                item,
                position: Position::ZERO,
            },
            out,
        )
    })
    .expect("load");
    let deadline = kithara_platform::time::WallInstant::now() + Duration::from_secs(5);
    let receipt = loop {
        if let Some(receipt) = dispatcher.receipts().next() {
            break receipt;
        }
        assert!(
            kithara_platform::time::WallInstant::now() < deadline,
            "real lane opens"
        );
        kithara_platform::tokio::task::yield_now().await;
    };
    with_mixer(&mut mixer, &mut dispatcher, frame(0), |out| {
        track.settle(TrackReceipt::Loaded(receipt), out)
    });
    let mut pcm = [[0.0; 64]; 2];
    let [left, right] = &mut pcm;
    mixer.block(frame(0), [left, right]).expect("attach block");
    settle_mixer(&mut track, &mut mixer, &mut dispatcher, frame(0));
    with_mixer(&mut mixer, &mut dispatcher, frame(64), |out| {
        track.apply(TrackCommand::Play { at: When::Next }, out)
    })
    .expect("play");
    let [left, right] = &mut pcm;
    mixer.block(frame(64), [left, right]).expect("start block");
    settle_mixer(&mut track, &mut mixer, &mut dispatcher, frame(64));
    let original = track.segment;
    with_mixer(&mut mixer, &mut dispatcher, frame(128), |out| {
        track.apply(TrackCommand::PlayAfter { track: A }, out)
    })
    .expect("park repeat");
    let deadline = kithara_platform::time::WallInstant::now() + Duration::from_secs(5);
    while frames == 441 && track.ready != Some(track.segment) {
        track.settle_lane();
        assert!(
            kithara_platform::time::WallInstant::now() < deadline,
            "lane switches to the repeated segment"
        );
        kithara_platform::tokio::task::yield_now().await;
    }
    kithara_platform::time::sleep(Duration::from_millis(10)).await;
    with_mixer(&mut mixer, &mut dispatcher, frame(128), |out| {
        track.apply(TrackCommand::Supersede, out)
    })
    .expect("cancel parked repeat");
    let mut ended = false;
    let blocks = frames.div_ceil(64) + 32;
    let mut tail_audible = false;
    for block in 2..blocks {
        let at = frame(i64::try_from(block * 64).expect("test frame"));
        let [left, right] = &mut pcm;
        mixer.block(at, [left, right]).expect("original tail block");
        settle_mixer(&mut track, &mut mixer, &mut dispatcher, at);
        let snapshot = mixer.ends.snapshot.read();
        assert!(
            snapshot.slots[0]
                .mark
                .is_none_or(|mark| mark.lane.segment == original),
            "the mixer never adopts the repeated segment"
        );
        if ended {
            assert!(
                pcm.iter().flatten().all(|sample| *sample == 0.0),
                "no frame of the repeat is heard"
            );
        }
        if frames > 441 && block * 64 >= frames - 4_410 && block * 64 < frames - 128 {
            tail_audible |= pcm.iter().flatten().any(|sample| *sample > 0.4);
        }
        for event in mixer.ends.events.drain().collect::<Vec<_>>() {
            ended |= matches!(event, DeckEvent::Ended { slot: A, .. });
            with_mixer(&mut mixer, &mut dispatcher, at, |out| {
                track.settle(TrackReceipt::Event(event), out)
            });
        }
        kithara_platform::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(ended, "the original segment publishes Ended");
    assert!(matches!(track.snapshot().status, TrackStatus::Ended { .. }));
    assert_eq!(track.segment, original);
    assert!(
        frames == 441 || tail_audible,
        "cancellation retains the original unread tail"
    );
    with_mixer(&mut mixer, &mut dispatcher, frame(2_048), |out| {
        track.apply(
            TrackCommand::Configure(TrackSettingsChange::Speed(1.25), When::Next),
            out,
        )
    })
    .expect("Configure after supersession is neither NotReady nor Late");
    with_mixer(&mut mixer, &mut dispatcher, frame(2_048), |out| {
        track.apply(TrackCommand::Seek { to: Position::ZERO }, out)
    })
    .expect("a later seek remains admissible");
    assert_eq!(
        track.segment,
        original.next().next(),
        "a cancelled segment's buffered PCM is never adopted by a later seek"
    );
    drop(driver);
}

pub(super) fn settle_mixer(
    track: &mut Track,
    mixer: &mut mock::MixerRig,
    dispatcher: &mut Sender<DispatcherProtocol<ResourceLoad<TestPools>>>,
    now: SessionFrame,
) {
    while let Some(receipt) = mixer.ring.receipt() {
        let kithara_command::ScopedReceipt::Scope(_, receipt) = receipt else {
            continue;
        };
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        with_mixer(mixer, dispatcher, now, |out| {
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

#[kithara::test]
fn a_track_whose_slot_faded_out_reports_it() {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "track");

    rig.with_outbox(|out| {
        track.settle(
            TrackReceipt::Event(DeckEvent::Faded {
                slot: A,
                at: frame(2_048),
            }),
            out,
        )
    })
    .expect("live deck scope");

    assert_eq!(
        track.snapshot().status,
        TrackStatus::Faded { at: frame(2_048) }
    );
}

#[kithara::test]
fn a_load_the_deck_has_no_room_for_leaves_the_lane_untouched() {
    let mut rig = rig();
    {
        let deck = &mut rig.deck;
        let mut scope = deck.ring.scope(deck.scope).expect("live deck scope");
        let error = loop {
            if let Err(error) = scope.send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: Vec::new(),
                },
            ) {
                break error;
            }
        };
        assert!(matches!(error, SendError::Full(_)), "{error:?}");
    }
    let (sender, mut inbox) = lane();
    let mut track = track(A);
    let seq = load(&mut track, &mut rig, Position::ZERO);
    track.lane = Some(sender);
    let opened = opened_fixture(&mut rig, "track");
    let receipt = rig.open(Ok(opened)).expect("one open");

    let settled = rig
        .with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope");
    rig.with_outbox(|out| {
        track.apply(
            TrackCommand::Configure(TrackSettingsChange::Speed(2.0), When::Next),
            out,
        )
    })
    .expect("live deck scope")
    .expect("a valid speed");

    assert!(
        matches!(
            settled,
            Settled::Rejected { seq: answered, reason: Rejection::Refused(PlayError::Full("deck")) }
                if answered == seq
        ),
        "{settled:?}"
    );
    assert_eq!(track.snapshot().status, TrackStatus::Idle);
    let commands = lane_commands(&mut inbox);
    assert!(commands.is_empty(), "{commands:?}");
}

#[kithara::test]
fn a_loaded_track_sends_each_change_to_its_lane() {
    let mut rig = rig();
    let (mut track, mut inbox) = loaded(&mut rig, A, "track");

    let sent = rig
        .with_outbox(|out| {
            track.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(2.0), When::Next),
                out,
            )
        })
        .expect("live deck scope")
        .expect("a valid speed");

    assert!(sent.is_some());
    let commands = lane_commands(&mut inbox);
    assert!(
        matches!(
            commands.as_slice(),
            [LaneCommand::SetSpeed(SpeedCurve::Constant(changed))] if *changed == 2.0
        ),
        "{commands:?}"
    );
    assert!(
        (track.snapshot().speed - 1.0).abs() < f32::EPSILON,
        "the speed shows once the lane applied it"
    );
    rig.with_outbox(|out| track.tick(frame(0), out))
        .expect("live deck scope");
    assert!((track.snapshot().speed - 2.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn full_notification_ring_retries_latest_effective_rate_once() {
    let mut rig = rig();
    let (mut track, _) = prepared_track();
    let (sender, mut inbox) = channel(
        ChannelConfig::builder()
            .capacity(std::num::NonZeroUsize::new(2).expect("two outstanding rates"))
            .build(),
    );
    track.lane = Some(sender);
    let first = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Configure(TrackSettingsChange::Speed(1.25), When::Next),
    )
    .expect("first rate is admitted");
    let latest = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Configure(TrackSettingsChange::Speed(1.5), When::Next),
    )
    .expect("latest rate is admitted");
    let commands = lane_commands(&mut inbox);
    assert!(
        matches!(commands.as_slice(), [LaneCommand::SetSpeed(SpeedCurve::Constant(first)),
        LaneCommand::SetSpeed(SpeedCurve::Constant(latest))] if *first == 1.25 && *latest == 1.5)
    );
    assert_ne!(first, latest);
    assert_eq!(track.lane.as_ref().expect("lane").available(), 0);
    assert_eq!(track.projected().speed(), 1.5);
    assert_eq!(track.snapshot().speed, 1.0);
    rig.with_outbox(|out| track.tick(frame(0), out))
        .expect("owner takes the reserved replies");
    assert_eq!(track.snapshot().speed, 1.5);
    assert_eq!(track.lane.as_ref().expect("lane").available(), 2);
    assert!(
        track
            .lane
            .as_mut()
            .expect("lane")
            .receipts()
            .next()
            .is_none()
    );
    assert_eq!(
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::Configure(TrackSettingsChange::Speed(1.5), When::Next)
        ),
        None
    );
    assert!(lane_commands(&mut inbox).is_empty());
    rig.with_outbox(|out| track.tick(frame(0), out))
        .expect("unchanged rate owner pass");
    assert_eq!(track.snapshot().speed, 1.5);
    assert!(
        track
            .lane
            .as_mut()
            .expect("lane")
            .receipts()
            .next()
            .is_none()
    );
}

#[kithara::test]
fn an_unchanged_speed_target_still_cancels_a_ramp() {
    let mut rig = rig();
    let (mut track, mut inbox) = prepared_track();
    assert!(
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::SetSpeed {
                speed: SpeedCurve::Ramp {
                    to: 1.5,
                    frames: std::num::NonZeroU64::new(128).expect("ramp frames"),
                },
                at: When::Next,
            }
        )
        .is_some()
    );
    assert!(matches!(
        lane_commands(&mut inbox).as_slice(),
        [LaneCommand::SetSpeed(SpeedCurve::Ramp { .. })]
    ));
    rig.with_outbox(|out| track.tick(frame(0), out))
        .expect("ramp applies");
    assert_eq!(track.snapshot().speed, 1.5);
    assert!(
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::Configure(TrackSettingsChange::Speed(1.5), When::Next,)
        )
        .is_some()
    );
    assert!(matches!(lane_commands(&mut inbox).as_slice(),
        [LaneCommand::SetSpeed(SpeedCurve::Constant(speed))] if *speed == 1.5));
}

#[kithara::test]
#[case::silent(false)]
#[case::loaded(true)]
fn a_change_at_a_frame_is_refused_as_untimed(#[case] attached: bool) {
    let mut rig = rig();
    let (mut track, mut inbox) = if attached {
        loaded(&mut rig, A, "track")
    } else {
        (track(A), lane().1)
    };

    let refused = rig
        .with_outbox(|out| {
            track.apply(
                TrackCommand::Configure(TrackSettingsChange::Keylock(true), When::At(frame(4_096))),
                out,
            )
        })
        .expect("live deck scope");

    assert!(matches!(refused, Err(PlayError::Untimed)), "{refused:?}");
    assert!(lane_commands(&mut inbox).is_empty());
    assert!(!track.snapshot().status.eq(&TrackStatus::Released));
}

#[kithara::test]
fn a_change_before_the_load_applies_at_once_and_starts_the_lane_there() {
    let mut rig = rig();
    let mut track = track(A);

    let sent = rig
        .with_outbox(|out| {
            track.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(1.25), When::Next),
                out,
            )
        })
        .expect("live deck scope")
        .expect("a valid speed");

    assert!(sent.is_none(), "no lane to send to yet");
    assert!((track.snapshot().speed - 1.25).abs() < f32::EPSILON);
}

#[kithara::test]
fn failed_deck_event_preserves_item_identity_and_the_first_terminal_cause() {
    use kithara_audio::TrackFailureKind;
    use kithara_render::bridge::PlaybackFault;
    let mut rig = rig();
    let mut player = track(A);
    let item = player.snapshot().item;
    player.status = TrackStatus::Playing { since: frame(0) };
    let fault = PlaybackFault::Source(TrackFailureKind::SourceCancelled);
    rig.with_outbox(|out| {
        player.settle(
            TrackReceipt::Event(DeckEvent::Failed {
                slot: A,
                at: frame(7),
                fault,
            }),
            out,
        )
    })
    .expect("live deck scope");
    let terminal = player.snapshot();
    assert_eq!(terminal.item, item);
    assert_eq!(terminal.slot, Some(A));
    assert_eq!(
        terminal.status,
        TrackStatus::Failed {
            at: frame(7),
            fault
        }
    );
    for event in [
        DeckEvent::Failed {
            slot: A,
            at: frame(8),
            fault: PlaybackFault::Source(TrackFailureKind::ChannelClosed),
        },
        DeckEvent::Ended {
            slot: A,
            at: frame(9),
        },
        DeckEvent::Failed {
            slot: B,
            at: frame(10),
            fault,
        },
    ] {
        rig.with_outbox(|out| player.settle(TrackReceipt::Event(event), out))
            .expect("live deck scope");
        assert_eq!(player.snapshot().item, item);
        assert_eq!(player.snapshot().status, terminal.status);
    }
    assert!(fault.to_string().contains("source cancelled"));
}

#[kithara::test]
#[case::stale(false)]
#[case::paused(true)]
fn stale_or_paused_failure_does_not_change_the_scoped_player(#[case] paused: bool) {
    use kithara_audio::{DecodeErrorKind, TrackFailureKind};
    use kithara_render::bridge::PlaybackFault;
    let mut rig = rig();
    let mut player = track(A);
    player.status = if paused {
        TrackStatus::Paused { at: Position::ZERO }
    } else {
        TrackStatus::Playing { since: frame(8) }
    };
    let before = player.snapshot();
    rig.with_outbox(|out| {
        player.settle(
            TrackReceipt::Event(DeckEvent::Failed {
                slot: A,
                at: frame(7),
                fault: PlaybackFault::Source(TrackFailureKind::Decode {
                    kind: DecodeErrorKind::InvalidData,
                }),
            }),
            out,
        )
    })
    .expect("live deck scope");
    assert_eq!(player.snapshot().item, before.item);
    assert_eq!(player.snapshot().status, before.status);
}
