use std::{
    collections::VecDeque,
    num::{NonZeroU16, NonZeroU32},
    sync::{Arc, Mutex},
    task::Waker,
};

use kithara_assets::AssetStore;
use kithara_audio::AudioObserverSlot;
use kithara_beat::{BeatGridModel, BeatGridState, GridBeat, GridDownbeat, Meter, RawBeatGrid};
use kithara_command::{
    Batch, ChannelConfig, Inbox, Outcome, Rejection, Sender, Seq, When, channel,
};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_play::{
    Bound, DeckMixSettings, DeckPass, HostedDeck, Outbox, OutputSnapshot, PlayError, PlayWorker,
    Player, Position, ResourceConfig, ResourceLoad, ResourceSrc, Settled, Track, TrackCommand,
    TrackReceipt, TrackSettings, TrackSettingsChange, TrackSnapshot, TrackStatus, mock::DeckRig,
};
use kithara_render::{
    bridge::{DeckPart, DeckProtocol, DeckSnapshot, Slot},
    rt::DeckMixerConfig,
};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::bufpool::{TestPools, pools};
use kithara_warp::{SessionBeat, SpeedCurve};
use num_traits::AsPrimitive;

use crate::{GridAnswer, LinkConfig, Linked, LinkedPlayer, TempoStep, TempoTrajectory};

pub(super) type Deck = Linked<ScriptedTrack>;

pub(super) fn rate(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("nonzero test rate")
}

pub(super) fn position(frames: u32) -> Position {
    Position::from_secs_f64(f64::from(frames) / 48_000.0)
}

pub(super) fn trajectory(bpm: f64, meter: u16) -> TempoTrajectory {
    TempoTrajectory::new(
        TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::default(),
            tempo: kithara_host::api::Tempo::new(bpm).expect("test tempo"),
        },
        NonZeroU16::new(meter).expect("test meter"),
        rate(48_000),
    )
}

pub(super) fn grid(
    beat_frames: u32,
    first: u32,
    meter: Option<(u16, i64)>,
    covered: u32,
) -> BeatGridModel {
    let beats: Vec<_> = (0..=(covered - first) / beat_frames)
        .map(|ordinal| GridBeat {
            confidence: None,
            at: f64::from(first + ordinal * beat_frames) / 48_000.0,
            ordinal: i64::from(ordinal),
        })
        .collect();
    let meter = meter.map(|(count, origin)| Meter {
        beats_per_bar: NonZeroU16::new(count).expect("test meter"),
        origin_beat_ordinal: origin,
    });
    let downbeats = beats
        .iter()
        .filter(|beat| {
            meter.is_some_and(|meter| {
                (beat.ordinal - meter.origin_beat_ordinal)
                    .rem_euclid(i64::from(meter.beats_per_bar.get()))
                    == 0
            })
        })
        .map(|beat| GridDownbeat {
            confidence: None,
            at: beat.at,
            beat_ordinal: beat.ordinal,
        })
        .collect();
    BeatGridModel::try_from(RawBeatGrid {
        state: BeatGridState::Final,
        duration: Some(f64::from(covered.max(480_000)) / 48_000.0),
        meter,
        model_id: "link-contract".into(),
        beats,
        downbeats,
        bpm: 60.0 * 48_000.0 / f64::from(beat_frames),
        schema_version: 1,
        revision: 0,
    })
    .expect("consistent test grid")
}

pub(super) fn resource() -> ResourceLoad<TestPools> {
    ResourceLoad::new(
        ResourceConfig::for_src(
            ResourceSrc::parse("https://example.com/track.mp3").expect("test source"),
        )
        .store(AssetStore::builder(pools()).build())
        .build(),
        Box::new(AudioObserverSlot::default().relay()),
    )
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Command {
    Load(Position),
    Play(When<SessionFrame>),
    Pause(When<SessionFrame>),
    Seek(Position),
    Jump(Position, SessionFrame),
    Speed(SpeedCurve, When<SessionFrame>),
    ConfigureSpeed(f32, When<SessionFrame>),
    Rate(NonZeroU32),
    Release,
}

pub(super) struct Script {
    pub snapshot: TrackSnapshot,
    pub sent: Vec<(Seq, Command)>,
    pub reject: Option<PlayError>,
    pending: Vec<(Seq, Vec<Command>, bool)>,
    speeds: VecDeque<Settled>,
    verdicts: Vec<(Seq, bool)>,
}

#[derive(Clone)]
pub(super) struct Control(Arc<Mutex<Script>>);

impl Control {
    pub(super) fn edit<R>(&self, edit: impl FnOnce(&mut Script) -> R) -> R {
        edit(&mut self.0.lock().expect("script lock"))
    }

    pub(super) fn commands(&self) -> Vec<(Seq, Command)> {
        self.edit(|script| script.sent.clone())
    }

    pub(super) fn clear(&self) {
        self.edit(|script| script.sent.clear());
    }
}

pub(super) struct Sequence {
    sender: Sender<DeckProtocol>,
    inbox: Inbox<DeckProtocol>,
}

impl Sequence {
    pub(super) fn new() -> Self {
        let (sender, inbox) = channel(ChannelConfig::builder().build());
        Self { sender, inbox }
    }

    pub(super) fn next(&mut self) -> Seq {
        let seq = self
            .sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: Vec::new(),
                },
            )
            .expect("sequence channel has room");
        self.inbox.drain();
        self.inbox
            .next_due(SessionFrame::new(0), 1)
            .expect("sent sequence")
            .apply(());
        let _ = self.sender.receipts().next();
        seq
    }
}

pub(super) struct ScriptedTrack {
    control: Control,
    sequence: Sequence,
}

impl ScriptedTrack {
    fn send(&mut self, commands: Vec<Command>) -> Result<Option<Seq>, PlayError> {
        self.control.edit(|script| {
            if let Some(error) = script.reject.take() {
                return Err(error);
            }
            let seq = self.sequence.next();
            script
                .sent
                .extend(commands.iter().cloned().map(|command| (seq, command)));
            script.pending.push((seq, commands, true));
            Ok(Some(seq))
        })
    }
}

impl Player<TestPools> for ScriptedTrack {
    type Command = TrackCommand<TestPools>;
    type Snapshot = TrackSnapshot;

    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        let (Bound::AtOrAfter(frame) | Bound::AtOrBefore(frame)) = bound;
        Some(frame)
    }

    fn apply(
        &mut self,
        command: Self::Command,
        _out: &mut Outbox<'_, TestPools>,
    ) -> Result<Option<Seq>, PlayError> {
        let command = match command {
            TrackCommand::Load { position, .. } => Command::Load(position),
            TrackCommand::Play { at } => Command::Play(at),
            TrackCommand::Pause { at } => Command::Pause(at),
            TrackCommand::Seek { to } => Command::Seek(to),
            TrackCommand::Jump { to, at } => Command::Jump(to, at),
            TrackCommand::SetSpeed { speed, at } => Command::Speed(speed, at),
            TrackCommand::Configure(TrackSettingsChange::Speed(speed), at) => {
                Command::ConfigureSpeed(speed, at)
            }
            TrackCommand::Configure(_, _) => {
                panic!("unexpected settings in synchronization fixture")
            }
            TrackCommand::SetHostRate { rate } => Command::Rate(rate),
            TrackCommand::Release => Command::Release,
            TrackCommand::Supersede
            | TrackCommand::Fade { .. }
            | TrackCommand::PlayAfter { .. }
            | TrackCommand::Seat { .. } => {
                panic!("unexpected command in synchronization fixture")
            }
        };
        let load = matches!(command, Command::Load(_));
        let sent = self.send(vec![command.clone()])?;
        if load {
            self.control.edit(|script| {
                script.pending.retain(|(seq, _, _)| Some(*seq) == sent);
                script.snapshot.status = TrackStatus::Loading;
                if let Command::Load(position) = command {
                    script.snapshot.position = position;
                }
            });
        }
        Ok(sent)
    }

    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, TestPools>,
        _out: &mut Outbox<'_, TestPools>,
    ) -> Settled {
        let TrackReceipt::Deck { seq, outcome, .. } = receipt else {
            return Settled::Pending;
        };
        self.control.edit(|script| {
            let Some(index) = script
                .pending
                .iter()
                .position(|(pending, _, _)| *pending == seq)
            else {
                return Settled::Pending;
            };
            let (_, commands, current) = script.pending.remove(index);
            if !current {
                return Settled::Rejected {
                    seq,
                    reason: Rejection::Stale,
                };
            }
            let speed = commands
                .iter()
                .any(|command| matches!(command, Command::Speed(..)));
            let settled = match outcome {
                Outcome::Applied { at, .. } => {
                    for command in commands {
                        match command {
                            Command::Load(position)
                            | Command::Seek(position)
                            | Command::Jump(position, _) => {
                                script.snapshot.position = position;
                                if matches!(script.snapshot.status, TrackStatus::Loading) {
                                    script.snapshot.status = TrackStatus::Loaded;
                                }
                            }
                            Command::Play(_) => {
                                script.snapshot.status = TrackStatus::Playing { since: *at }
                            }
                            Command::Pause(_) => {
                                script.snapshot.status = TrackStatus::Paused {
                                    at: script.snapshot.position,
                                }
                            }
                            Command::Speed(curve, _) => {
                                script.snapshot.speed = match curve {
                                    SpeedCurve::Constant(value) => value,
                                    SpeedCurve::Steps(steps) => {
                                        steps.last().expect("nonempty speed curve").1
                                    }
                                    _ => panic!("unexpected speed curve"),
                                }
                            }
                            Command::ConfigureSpeed(value, _) => script.snapshot.speed = value,
                            Command::Release => script.snapshot.status = TrackStatus::Released,
                            Command::Rate(_) => {}
                        }
                    }
                    script.snapshot.pending_lane = false;
                    Settled::Applied { seq, at: *at }
                }
                Outcome::Rejected(reason) => Settled::Rejected {
                    seq,
                    reason: match reason {
                        Rejection::Late => Rejection::Late,
                        Rejection::Stale => Rejection::Stale,
                        Rejection::Unanswered => Rejection::Unanswered,
                        Rejection::Refused(_) => Rejection::Refused(PlayError::Full("lane")),
                    },
                },
            };
            if speed {
                script
                    .verdicts
                    .push((seq, matches!(settled, Settled::Applied { .. })));
                script.speeds.push_back(settled.clone());
            }
            settled
        })
    }

    fn tick(&mut self, _now: SessionFrame, _out: &mut Outbox<'_, TestPools>) {}

    fn snapshot(&self) -> Self::Snapshot {
        self.control.edit(|script| script.snapshot.clone())
    }
}

impl Track<TestPools> for ScriptedTrack {
    fn admit(
        &mut self,
        _change: TrackSettingsChange,
        _at: When<SessionFrame>,
        _out: &Outbox<'_, TestPools>,
    ) -> Result<(), PlayError> {
        Ok(())
    }

    fn projected(&self) -> TrackSettings {
        TrackSettings::builder()
            .speed(self.snapshot().speed)
            .build()
    }

    fn planned(
        &self,
        at: SessionFrame,
        sample_rate: NonZeroU32,
    ) -> Result<(Position, f32), PlayError> {
        let track = self.snapshot();
        if !track.attached {
            return Err(PlayError::NotReady);
        }
        let TrackStatus::Playing { since } = track.status else {
            return Ok((track.position, track.speed));
        };
        let frames = at.frames_since(since).ok_or(PlayError::Late)?;
        let frames: f64 = frames.as_();
        let elapsed =
            Position::from_secs_f64(frames * f64::from(track.speed) / f64::from(sample_rate.get()));
        Ok((track.position + elapsed, track.speed))
    }

    fn planned_end(&self, _rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError> {
        Ok(None)
    }
    fn speed_receipt(&mut self) -> Option<Settled> {
        self.control.edit(|script| script.speeds.pop_front())
    }
    fn speed_applied(&mut self, seq: Seq) -> Option<bool> {
        self.control.edit(|script| {
            script
                .verdicts
                .iter()
                .find(|(named, _)| *named == seq)
                .map(|(_, applied)| *applied)
        })
    }
    fn finish_group(&mut self, _result: Result<Seq, &mut Vec<DeckPart>>) {}

    fn cue(
        &mut self,
        position: Position,
        speed: f32,
        _out: &mut Outbox<'_, TestPools>,
    ) -> Result<Option<Seq>, PlayError> {
        let sent = self.send(vec![
            Command::Speed(SpeedCurve::Constant(speed), When::Next),
            Command::Seek(position),
        ])?;
        self.control.edit(|script| {
            for (_, commands, current) in &mut script.pending {
                if commands
                    .iter()
                    .any(|command| matches!(command, Command::Play(_)))
                {
                    *current = false;
                }
            }
            script.snapshot.pending_lane = true;
        });
        Ok(sent)
    }
}

impl HostedDeck<TestPools> for ScriptedTrack {
    fn worker(&self) -> Option<&PlayWorker<TestPools>> {
        None
    }
    fn mixer_config(&self) -> DeckMixerConfig {
        DeckMixerConfig::default()
    }
    fn drain(&mut self, _pass: DeckPass<'_>, _out: &mut Outbox<'_, TestPools>) {}
    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, TestPools>,
        _pass: DeckPass<'_>,
        out: &mut Outbox<'_, TestPools>,
    ) {
        Player::settle(self, receipt, out);
    }
    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, TestPools>) {
        Player::tick(self, pass.now, out);
    }
    fn close(&mut self, out: &mut Outbox<'_, TestPools>) -> Result<(), PlayError> {
        self.apply(TrackCommand::Release, out).map(|_| ())
    }
    fn hold(&mut self, _waker: Waker) {}
    fn release(&mut self) {}
}

pub(super) struct Rig {
    pub queues: DeckRig<TestPools>,
    pub now: SessionFrame,
    pub delivery: FrameCount,
    output: OutputSnapshot,
    observation: DeckSnapshot,
}

impl Rig {
    pub(super) fn new(now: i64) -> Self {
        Self {
            queues: DeckRig::new(DeckMixerConfig::default()).expect("deck scope"),
            now: SessionFrame::new(now),
            delivery: FrameCount::new(128),
            output: kithara_play::mock::output(None).get(),
            observation: DeckSnapshot::default(),
        }
    }

    pub(super) fn run<R>(&mut self, run: impl FnOnce(&mut Outbox<'_, TestPools>) -> R) -> R {
        self.run_pass(|out, _pass| run(out))
    }

    pub(super) fn run_pass<R>(
        &mut self,
        run: impl FnOnce(&mut Outbox<'_, TestPools>, DeckPass<'_>) -> R,
    ) -> R {
        let mut port = self
            .queues
            .ring
            .scope(self.queues.scope)
            .expect("live deck scope");
        let pass = DeckPass {
            mix: DeckMixSettings::default(),
            suspended: false,
            now: self.now,
            delivery: self.delivery,
            output: &self.output,
            deck: &self.observation,
        };
        run(
            &mut Outbox::new(&mut port, &mut self.queues.dispatcher).in_pass(pass),
            pass,
        )
    }

    pub(super) fn settle(
        &mut self,
        deck: &mut Deck,
        seq: Seq,
        outcome: &Outcome<DeckProtocol>,
    ) -> Settled {
        let mut batch = Batch {
            basis: vec![(Slot::new(0), None)],
            commands: Vec::new(),
        };
        self.run(|out| {
            Player::settle(
                deck,
                TrackReceipt::Deck {
                    seq,
                    outcome,
                    batch: &mut batch,
                },
                out,
            )
        })
    }

    pub(super) fn apply(&mut self, deck: &mut Deck, seq: Seq, frame: i64) -> Settled {
        self.settle(
            deck,
            seq,
            &Outcome::Applied {
                at: SessionFrame::new(frame),
                data: (),
            },
        )
    }
}

pub(super) fn deck(host: TempoTrajectory) -> (Deck, Control) {
    let control = Control(Arc::new(Mutex::new(Script {
        snapshot: TrackSnapshot {
            item: TrackId::allocate(),
            slot: Some(Slot::new(0)),
            status: TrackStatus::Loaded,
            speed: 1.0,
            position: Position::ZERO,
            duration: None,
            abr: None,
            metadata: TrackMetadata::default(),
            mark: None,
            engine_latency: FrameCount::new(0),
            ring_depth: FrameCount::new(0),
            lane_room: 32,
            pending_lane: false,
            attached: true,
            declick: FrameCount::new(0),
        },
        sent: Vec::new(),
        reject: None,
        pending: Vec::new(),
        speeds: VecDeque::new(),
        verdicts: Vec::new(),
    })));
    (
        Linked::new(
            ScriptedTrack {
                control: control.clone(),
                sequence: Sequence::new(),
            },
            LinkConfig::default(),
            host,
        ),
        control,
    )
}

pub(super) fn load(deck: &mut Deck, rig: &mut Rig, cue: u32) -> Seq {
    rig.run(|out| {
        deck.apply(
            TrackCommand::Load {
                item: resource(),
                position: position(cue),
            },
            out,
        )
    })
    .expect("load accepted")
    .expect("load sequence")
}

pub(super) fn answer(deck: &mut Deck, rig: &mut Rig, load: Seq, model: BeatGridModel) {
    let item = deck.snapshot().as_ref().item;
    rig.run(|out| {
        LinkedPlayer::grid(
            deck,
            GridAnswer {
                item,
                load,
                model: Ok(model),
            },
            out,
        );
    });
}

pub(super) fn loaded(model: BeatGridModel) -> (Deck, Control, Rig, Seq) {
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(0);
    let load = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, load, 0);
    answer(&mut deck, &mut rig, load, model);
    control.clear();
    (deck, control, rig, load)
}

pub(super) fn sounding() -> (Deck, Control, Rig, Seq) {
    let (mut deck, control, mut rig, load) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync accepted")
        .expect("speed sequence");
    rig.apply(&mut deck, seq, 0);
    control.edit(|script| {
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(0),
        }
    });
    control.clear();
    (deck, control, rig, load)
}
