use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
    task::Waker,
};

use kithara_command::{Rejection, Seq, When};
use kithara_host::{
    DeckId, HostCommand, HostOwner, HostSettingsChange, HostSettingsExec, HostSettled, api::Tempo,
};
use kithara_play::{
    DeckPass, HostedDeck, Outbox, PlayError, PlayWorker, Player, SessionTransportSnapshot,
    TrackCommand, TrackReceipt, TrackSnapshot,
};
use kithara_render::rt::DeckMixerConfig;
use kithara_signal::{FrameCount, SessionEpoch, SessionFrame, TransportRevision};
use kithara_test_utils::bufpool::TestPools;
use kithara_warp::{BeatGridRevision, BeatGridStamp, SessionAnchor, SessionBeat};

use super::fixtures::{Control, Deck, Rig, Sequence, rate, trajectory};
use crate::{GridAnswer, LinkedDeck, LinkedHost, LinkedPlayer, LinkedSnapshot, TempoTrajectory};

pub(super) struct Observation {
    pub snapshot: LinkedSnapshot<TrackSnapshot>,
    pub trajectories: Vec<(SessionFrame, TempoTrajectory)>,
    commands: Vec<TrackCommand<TestPools>>,
}

#[derive(Clone)]
pub(super) struct Probe(Arc<Mutex<Observation>>);

impl Probe {
    pub(super) fn read<R>(&self, read: impl FnOnce(&mut Observation) -> R) -> R {
        read(&mut self.0.lock().expect("deck observation lock"))
    }

    pub(super) fn trajectory(&self) -> TempoTrajectory {
        self.read(|observation| {
            observation
                .trajectories
                .last()
                .expect("Host delivered trajectory")
                .1
                .clone()
        })
    }

    pub(super) fn clear(&self) {
        self.read(|observation| observation.trajectories.clear());
    }

    pub(super) fn send(&self, command: TrackCommand<TestPools>) {
        self.read(|observation| observation.commands.push(command));
    }
}

struct ObservedDeck {
    inner: Deck,
    probe: Probe,
}

impl ObservedDeck {
    fn publish(&self) {
        self.probe
            .read(|observation| observation.snapshot = self.inner.snapshot());
    }
}

impl HostedDeck<TestPools> for ObservedDeck {
    delegate::delegate! {
        to self.inner {
            fn worker(&self) -> Option<&PlayWorker<TestPools>>;
            fn mixer_config(&self) -> DeckMixerConfig;
            fn close(&mut self, out: &mut Outbox<'_, TestPools>) -> Result<(), PlayError>;
            fn hold(&mut self, waker: Waker);
            fn release(&mut self);
        }
    }

    fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, TestPools>) {
        for command in self
            .probe
            .read(|observation| std::mem::take(&mut observation.commands))
        {
            self.inner.apply(command, out).expect("queued deck command");
        }
        self.inner.drain(pass, out);
        self.publish();
    }
    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, TestPools>,
        pass: DeckPass<'_>,
        out: &mut Outbox<'_, TestPools>,
    ) {
        HostedDeck::settle(&mut self.inner, receipt, pass, out);
        self.publish();
    }
    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, TestPools>) {
        HostedDeck::tick(&mut self.inner, pass, out);
        self.publish();
    }
}

impl LinkedDeck<TestPools> for ObservedDeck {
    fn sync(
        &mut self,
        on: bool,
        out: &mut Outbox<'_, TestPools>,
    ) -> Result<Option<Seq>, PlayError> {
        let sent = LinkedPlayer::sync(&mut self.inner, on, out);
        self.publish();
        sent
    }
    fn retime(
        &mut self,
        trajectory: &TempoTrajectory,
        at: SessionFrame,
        out: &mut Outbox<'_, TestPools>,
    ) {
        self.probe
            .read(|observation| observation.trajectories.push((at, trajectory.clone())));
        LinkedPlayer::retime(&mut self.inner, trajectory, at, out);
        self.publish();
    }
    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, TestPools>) {
        LinkedPlayer::grid(&mut self.inner, answer, out);
        self.publish();
    }
    fn synced(&self) -> bool {
        LinkedPlayer::synced(&self.inner)
    }
    fn lead(&self, delivery: FrameCount) -> Option<FrameCount> {
        LinkedPlayer::lead(&self.inner, delivery)
    }
    fn lane_room(&self) -> usize {
        LinkedPlayer::lane_room(&self.inner)
    }
    fn scope_parts(&self) -> usize {
        LinkedPlayer::scope_parts(&self.inner)
    }
    fn retime_applied(&mut self, at: SessionFrame) -> Option<bool> {
        LinkedPlayer::retime_applied(&mut self.inner, at)
    }
    fn realign(
        &mut self,
        trajectory: &TempoTrajectory,
        at: SessionFrame,
        out: &mut Outbox<'_, TestPools>,
    ) {
        self.probe
            .read(|observation| observation.trajectories.push((at, trajectory.clone())));
        LinkedPlayer::realign(&mut self.inner, trajectory, at, out);
        self.publish();
    }
}

pub(super) struct HostScript {
    pub clock: Option<(SessionFrame, FrameCount)>,
    pub transport: Option<SessionTransportSnapshot>,
    pub room: usize,
    pub reject: Option<PlayError>,
    pub sent: Vec<(Seq, Tempo, When<SessionFrame>)>,
    pub answers: Vec<HostSettled>,
}

#[derive(Clone)]
pub(super) struct HostControl(Arc<Mutex<HostScript>>);

impl HostControl {
    pub(super) fn edit<R>(&self, edit: impl FnOnce(&mut HostScript) -> R) -> R {
        edit(&mut self.0.lock().expect("Host script lock"))
    }

    pub(super) fn at(&self, now: i64) {
        self.edit(|script| script.clock = Some((SessionFrame::new(now), FrameCount::new(128))));
    }

    pub(super) fn axis(&self, epoch: u64, frame: i64, beat: f64, bpm: f64, sample_rate: u32) {
        let position = SessionBeat::new(beat).expect("axis beat");
        let tempo = Tempo::new(bpm).expect("axis tempo");
        let anchor = SessionAnchor::new(
            SessionFrame::new(frame),
            position,
            bpm / 60.0,
            rate(sample_rate),
        )
        .expect("axis anchor");
        self.edit(|script| {
            script.transport = Some(SessionTransportSnapshot::new(
                position,
                tempo,
                TransportRevision::first(),
                anchor,
                BeatGridStamp::new(
                    DeckId::allocate().expect("axis identity"),
                    BeatGridRevision::first(),
                ),
                SessionEpoch::new(epoch),
            ));
        });
        self.at(frame);
    }

    pub(super) fn answer(
        &self,
        seq: Seq,
        bpm: f64,
        outcome: Result<SessionFrame, Rejection<PlayError>>,
    ) {
        self.edit(|script| {
            script.answers.push(HostSettled::Settings {
                seq,
                change: HostSettingsChange::Tempo(Tempo::new(bpm).expect("receipt tempo")),
                outcome,
            });
        });
    }
}

struct Registered {
    id: DeckId,
    deck: Box<dyn LinkedDeck<TestPools>>,
    rig: Rig,
}

pub(super) struct ScriptedHost {
    control: HostControl,
    decks: Vec<Registered>,
    sequence: Sequence,
}

impl HostSettingsExec<()> for ScriptedHost {
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;
    fn exec_sample_rate(
        &mut self,
        _value: NonZeroU32,
        _at: Self::At,
        _cx: &mut (),
    ) -> Self::Output {
        Ok(None)
    }
    fn exec_live(
        &mut self,
        _change: HostSettingsChange,
        _at: Self::At,
        _cx: &mut (),
    ) -> Self::Output {
        Ok(None)
    }
    fn exec_tempo(&mut self, value: Tempo, at: Self::At, _cx: &mut ()) -> Self::Output {
        self.control.edit(|script| {
            if let Some(error) = script.reject.take() {
                return Err(error);
            }
            let seq = self.sequence.next();
            script.sent.push((seq, value, at));
            Ok(Some(seq))
        })
    }
}

impl HostOwner<TestPools> for ScriptedHost {
    type Command = HostCommand<TestPools, dyn LinkedDeck<TestPools>>;
    type Deck = dyn LinkedDeck<TestPools>;
    fn apply(&mut self, command: Self::Command) -> Result<Option<Seq>, PlayError> {
        match command {
            HostCommand::Register { id, deck } => self.register(id, deck).map(|()| None),
            HostCommand::Configure(change, at) => self.exec(change, at, &mut ()),
            _ => panic!("unexpected Host command"),
        }
    }
    fn register(&mut self, id: DeckId, deck: Box<Self::Deck>) -> Result<(), PlayError> {
        self.decks.push(Registered {
            id,
            deck,
            rig: Rig::new(0),
        });
        Ok(())
    }
    fn each_deck(
        &mut self,
        visit: &mut dyn FnMut(DeckId, &mut Self::Deck, &mut Outbox<'_, TestPools>, DeckPass<'_>),
    ) {
        let (now, delivery) = self
            .clock()
            .unwrap_or((SessionFrame::new(0), FrameCount::new(128)));
        for record in &mut self.decks {
            record.rig.now = now;
            record.rig.delivery = delivery;
            record
                .rig
                .run_pass(|out, pass| visit(record.id, &mut *record.deck, out, pass));
        }
    }
    fn with_deck(
        &mut self,
        id: DeckId,
        visit: &mut dyn FnMut(&mut Self::Deck, &mut Outbox<'_, TestPools>, DeckPass<'_>),
    ) -> Result<(), PlayError> {
        let (now, delivery) = self
            .clock()
            .unwrap_or((SessionFrame::new(0), FrameCount::new(128)));
        let record = self
            .decks
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or(PlayError::NotReady)?;
        record.rig.now = now;
        record.rig.delivery = delivery;
        record
            .rig
            .run_pass(|out, pass| visit(&mut *record.deck, out, pass));
        Ok(())
    }
    fn clock(&self) -> Option<(SessionFrame, FrameCount)> {
        self.control.edit(|script| script.clock)
    }
    fn transport(&mut self) -> Option<SessionTransportSnapshot> {
        self.control.edit(|script| script.transport)
    }
    fn host_room(&self) -> usize {
        self.control.edit(|script| script.room)
    }
    fn begin_pass(&mut self) {}
    fn prepare_offline(&mut self) -> Result<(), PlayError> {
        Ok(())
    }
    fn render_offline(
        &mut self,
        _position: u64,
        _frames: usize,
        _output: &mut [f32],
    ) -> Result<(), PlayError> {
        panic!("owner contracts do not render")
    }
    fn release_id(_command: &Self::Command) -> Option<DeckId> {
        None
    }
    fn is_next_tempo(_command: &Self::Command) -> bool {
        false
    }
    fn pass(&mut self) -> Vec<HostSettled> {
        self.control
            .edit(|script| std::mem::take(&mut script.answers))
    }
}

pub(super) type Host = LinkedHost<TestPools, ScriptedHost>;

pub(super) fn host() -> (Host, HostControl) {
    let control = HostControl(Arc::new(Mutex::new(HostScript {
        clock: Some((SessionFrame::new(0), FrameCount::new(128))),
        transport: None,
        room: 32,
        reject: None,
        sent: Vec::new(),
        answers: Vec::new(),
    })));
    (
        LinkedHost::new(
            ScriptedHost {
                control: control.clone(),
                decks: Vec::new(),
                sequence: Sequence::new(),
            },
            trajectory(120.0, 4),
        ),
        control,
    )
}

pub(super) fn register(host: &mut Host, deck: Deck, control: &Control) -> (DeckId, Probe) {
    let probe = Probe(Arc::new(Mutex::new(Observation {
        snapshot: deck.snapshot(),
        trajectories: Vec::new(),
        commands: Vec::new(),
    })));
    let id = DeckId::allocate().expect("deck identity");
    let deck: Box<dyn LinkedDeck<TestPools>> = Box::new(ObservedDeck {
        inner: deck,
        probe: probe.clone(),
    });
    host.apply(HostCommand::Register { id, deck }.into())
        .expect("deck registered");
    control.clear();
    (id, probe)
}

pub(super) fn tempo(host: &mut Host, bpm: f64, frame: i64) -> Result<Option<Seq>, PlayError> {
    host.apply(
        HostCommand::Configure(
            HostSettingsChange::Tempo(Tempo::new(bpm).expect("test tempo")),
            When::At(SessionFrame::new(frame)),
        )
        .into(),
    )
}

pub(super) fn finish(host: &mut Host) -> Vec<HostSettled> {
    assert!(
        host.pass().is_empty(),
        "tempo verdict is held until the owner pass"
    );
    host.begin_pass();
    host.pass()
}
