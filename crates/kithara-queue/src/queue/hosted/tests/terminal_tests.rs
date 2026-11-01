use std::num::NonZeroU32;

use kithara_audio::{DecodeErrorKind, TrackFailureKind};
use kithara_command::{Batch, ChannelConfig, Outcome, Seq, When, channel};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_play::{
    Bound, DeckEvent, DeckPart, Outbox, PlayError, PlaybackFault, Player, PlayerConfig, Settled,
    Slot, Track, TrackCommand, TrackFactory, TrackReceipt, TrackSettings, TrackSettingsChange,
    TrackSnapshot, TrackStatus as PlayerTrackStatus,
};
use kithara_signal::{AudioSpec, FrameCount, SessionFrame};
use kithara_test_utils::kithara;

use super::*;
use crate::{
    CrossfadeSettings, ItemEvent, PlaybackOrder, QueueConfig, QueueSettings, TrackSource,
    Transition, queue::slots::Active, test_pools::TestPools, track::TrackRecord,
};

struct SnapshotTrack(
    TrackSnapshot,
    Vec<(TrackSettingsChange, When<SessionFrame>)>,
    Option<usize>,
);
struct SnapshotFactory;

impl TrackFactory<TestPools> for SnapshotFactory {
    type Track = SnapshotTrack;
    fn track(&self, config: PlayerConfig) -> Result<Self::Track, PlayError> {
        Ok(SnapshotTrack(
            TrackSnapshot {
                effects: Vec::new(),
                item: config.item,
                slot: config.slot,
                status: PlayerTrackStatus::Loaded,
                speed: 1.0,
                position: Duration::ZERO,
                duration: None,
                abr: None,
                metadata: TrackMetadata::default(),
                mark: None,
                engine_latency: FrameCount::new(0),
                ring_depth: FrameCount::new(0),
                lane_room: 0,
                pending_lane: false,
                attached: true,
                declick: FrameCount::new(0),
            },
            Vec::new(),
            Some(0),
        ))
    }
}

impl Player<TestPools> for SnapshotTrack {
    type Command = TrackCommand<TestPools>;
    type Snapshot = TrackSnapshot;
    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        Some(match bound {
            Bound::AtOrAfter(at) | Bound::AtOrBefore(at) => at,
        })
    }
    fn apply(
        &mut self,
        command: Self::Command,
        out: &mut Outbox<'_, TestPools>,
    ) -> Result<Option<Seq>, PlayError> {
        match command {
            TrackCommand::Configure(change, when) => {
                self.admit(change, when, out)?;
                self.1.push((change, when));
                if matches!(change, TrackSettingsChange::Speed(speed) if speed != self.0.speed) {
                    return Ok(Some(fixture_seq()));
                }
            }
            TrackCommand::Pause { .. } => {
                self.0.status = PlayerTrackStatus::Paused { at: Duration::ZERO };
                return Ok(Some(fixture_seq()));
            }
            _ => {}
        }
        Ok(None)
    }
    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, TestPools>,
        _out: &mut Outbox<'_, TestPools>,
    ) -> Settled {
        if let TrackReceipt::Deck {
            seq,
            outcome: Outcome::Applied { at, .. },
            batch,
        } = receipt
            && batch
                .commands
                .iter()
                .any(|part| matches!(part, DeckPart::Detach { slot } if Some(*slot) == self.0.slot))
        {
            self.0.status = PlayerTrackStatus::Released;
            self.0.attached = false;
            return Settled::Applied { seq, at: *at };
        }
        Settled::Pending
    }
    fn tick(&mut self, _now: SessionFrame, _out: &mut Outbox<'_, TestPools>) {}
    fn snapshot(&self) -> Self::Snapshot {
        self.0.clone()
    }
}

impl Track<TestPools> for SnapshotTrack {
    fn admit(
        &mut self,
        _change: TrackSettingsChange,
        at: When<SessionFrame>,
        _out: &Outbox<'_, TestPools>,
    ) -> Result<Option<usize>, PlayError> {
        if self.0.lane_room == 0 {
            return Err(PlayError::Full("lane"));
        }
        if matches!(at, When::At(_)) && !matches!(self.0.status, PlayerTrackStatus::Playing { .. })
        {
            return Err(PlayError::Untimed);
        }
        Ok(self.2)
    }

    fn projected(&self) -> TrackSettings {
        TrackSettings::default()
    }

    fn prepared_speed(&self) -> Option<&kithara_sync::SpeedCurve> {
        None
    }

    fn prepare_entry(
        &mut self,
        at: SessionFrame,
        _out: &mut Outbox<'_, TestPools>,
    ) -> Result<bool, PlayError> {
        Ok(!self.0.pending_lane && self.entry(Bound::AtOrAfter(at)).is_some())
    }

    fn planned(
        &self,
        _at: SessionFrame,
        _sample_rate: NonZeroU32,
    ) -> Result<(Duration, f64), PlayError> {
        panic!("terminal-event fixture does not project a render lane")
    }

    fn planned_end(&self, sample_rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError> {
        let Some(duration) = self.0.duration else {
            return Ok(None);
        };
        let frames = AudioSpec::new(1, sample_rate)
            .frames_for(duration)
            .expect("fixture duration fits frames");
        Ok(Some(SessionFrame::new(
            i64::try_from(frames.get()).expect("fixture end fits i64"),
        )))
    }

    fn speed_receipt(&mut self) -> Option<Settled> {
        panic!("terminal-event fixture does not send speed commands")
    }

    fn speed_applied(&mut self, _seq: Seq) -> Option<bool> {
        panic!("terminal-event fixture does not send speed commands")
    }

    fn finish_group(&mut self, _result: Result<Seq, &mut Vec<DeckPart>>) {
        panic!("terminal-event fixture does not stage attachments")
    }

    fn cue(
        &mut self,
        _position: Duration,
        _speed: kithara_sync::SpeedCurve,
        _out: &mut Outbox<'_, TestPools>,
    ) -> Result<Option<Seq>, PlayError> {
        panic!("terminal-event fixture does not send cue commands")
    }
}

fn fixture_seq() -> Seq {
    let (mut sender, _inbox) =
        channel::<kithara_play::DeckProtocol>(ChannelConfig::builder().build());
    sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: Vec::new(),
            },
        )
        .expect("fixture sequence")
}

type TestQueue = Queue<TestPools, SnapshotFactory>;

#[kithara::test]
#[case::fade(false)]
#[case::chain(true)]
fn entry_preparation_preserves_the_queue_target_before_any_transition_part(#[case] auto: bool) {
    let (mut queue, incoming, current) = selected_second();
    let receiver = queue.active.get_mut(0).expect("incoming");
    receiver.role = Role::Incoming;
    receiver.track.0.status = PlayerTrackStatus::Loaded;
    receiver.track.0.pending_lane = true;
    queue.target = Some(super::super::super::types::Target {
        to: incoming,
        bound: Some(Bound::AtOrBefore(SessionFrame::new(512))),
        settings: CrossfadeSettings {
            duration: 0.0,
            ..CrossfadeSettings::default()
        },
        transition: Transition::None,
        reason: crate::AdvanceReason::NaturalEof,
        playing: true,
        auto,
        stale: None,
        continued: None,
        repeat: None,
        chained: false,
    });
    let (mut deck, mut inbox) = channel(ChannelConfig::builder().build());
    let (mut dispatcher, _dispatcher_inbox) = channel(ChannelConfig::builder().build());
    assert_eq!(
        queue
            .send_transition(
                0,
                SessionFrame::new(512),
                &mut Outbox::new(&mut deck, &mut dispatcher)
            )
            .expect("preparation wait"),
        None
    );
    assert_eq!(queue.current, Some(current));
    assert_eq!(queue.target.expect("target remains").to, incoming);
    assert_eq!(
        queue.target.expect("target bound remains").bound,
        Some(Bound::AtOrBefore(SessionFrame::new(512)))
    );
    assert!(queue.pending_handover.is_none());
    inbox.drain();
    assert!(
        inbox.next_due(SessionFrame::new(512), 1).is_none(),
        "neither half of the transition is sent before entry readiness"
    );
}

#[kithara::test]
fn a_track_settings_change_with_one_full_lane_reaches_no_track() {
    let (mut queue, _, _) = selected_second();
    let outgoing = 0;
    let current = 1;
    queue
        .active
        .get_mut(current)
        .expect("current")
        .track
        .0
        .lane_room = 4;
    let (mut deck, _deck_inbox) = channel(ChannelConfig::builder().build());
    let (mut dispatcher, _dispatcher_inbox) = channel(ChannelConfig::builder().build());
    let mut out = Outbox::new(&mut deck, &mut dispatcher);
    let error = queue.apply_command(
        QueueCommand::ConfigureTrack(TrackSettingsChange::Speed(1.5), When::Next),
        None,
        &mut out,
    );
    assert!(matches!(
        error,
        Err(QueueError::Play(PlayError::Full("lane")))
    ));
    for index in [outgoing, current] {
        assert!(
            queue
                .active
                .get(index)
                .expect("receiver")
                .track
                .1
                .is_empty()
        );
    }
    assert_eq!(queue.config.track.speed(), 1.0);
    let silent = &mut queue.active.get_mut(outgoing).expect("outgoing").track;
    silent.0.lane_room = 4;
    silent.0.status = PlayerTrackStatus::Loaded;
    let at = When::At(SessionFrame::new(128));
    queue
        .apply_command(
            QueueCommand::ConfigureTrack(TrackSettingsChange::Speed(1.5), at),
            None,
            &mut out,
        )
        .expect("all receivers admit");
    assert!(
        matches!(queue.active.get(current).expect("current").track.1.as_slice(),
        [(TrackSettingsChange::Speed(1.5), sent_at)] if *sent_at == at)
    );
    assert!(matches!(
        queue
            .active
            .get(outgoing)
            .expect("silent")
            .track
            .1
            .as_slice(),
        [(TrackSettingsChange::Speed(1.5), When::Next)]
    ));
    assert_eq!(queue.config.track.speed(), 1.5);
    drop(out);

    let (mut queue, _, _) = selected_second();
    for index in [outgoing, current] {
        let track = &mut queue.active.get_mut(index).expect("receiver").track;
        track.0.lane_room = 4;
        track.0.status = PlayerTrackStatus::Loaded;
        track.2 = Some(1);
    }
    let (mut deck, _deck_inbox) = channel(
        ChannelConfig::builder()
            .capacity(std::num::NonZeroUsize::MIN)
            .build(),
    );
    let mut out = Outbox::new(&mut deck, &mut dispatcher);
    let error = queue.apply_command(
        QueueCommand::ConfigureTrack(TrackSettingsChange::Speed(2.0), When::Next),
        None,
        &mut out,
    );
    assert!(matches!(
        error,
        Err(QueueError::Play(PlayError::Full("deck")))
    ));
    for index in [outgoing, current] {
        assert!(
            queue
                .active
                .get(index)
                .expect("unmodified receiver")
                .track
                .1
                .is_empty()
        );
    }
    assert_eq!(queue.config.track.speed(), 1.0);
}

#[kithara::test]
#[case(true)]
#[case(false)]
fn a_speed_change_on_the_current_track_withdraws_a_sent_automatic_transition(#[case] auto: bool) {
    let (mut queue, incoming, _) = selected_second();
    let batch = fixture_seq();
    let receiver = queue.active.get_mut(0).expect("incoming");
    receiver.role = Role::Incoming;
    receiver.track.0.status = PlayerTrackStatus::Loaded;
    receiver.track.0.lane_room = 4;
    queue.pending_handover = Some(super::super::super::types::PendingHandover {
        seq: batch,
        item: incoming,
        slot: receiver.slot,
        replacement: false,
        reason: crate::AdvanceReason::NaturalEof,
    });
    queue.active.get_mut(1).expect("current").track.0.lane_room = 4;
    queue.target = Some(super::super::super::types::Target {
        to: incoming,
        bound: Some(Bound::AtOrBefore(SessionFrame::new(512))),
        settings: CrossfadeSettings::default(),
        transition: Transition::None,
        reason: crate::AdvanceReason::NaturalEof,
        playing: true,
        auto,
        stale: None,
        continued: None,
        repeat: None,
        chained: false,
    });
    let (mut deck, _deck_inbox) = channel(ChannelConfig::builder().build());
    let (mut dispatcher, _dispatcher_inbox) = channel(ChannelConfig::builder().build());
    queue
        .apply_command(
            QueueCommand::ConfigureTrack(TrackSettingsChange::Speed(2.0), When::Next),
            None,
            &mut Outbox::new(&mut deck, &mut dispatcher),
        )
        .expect("broadcast accepted");
    let target = queue.target.expect("transition retained");
    assert_eq!(target.to, incoming);
    assert_eq!(target.stale, auto.then_some(batch));
    assert_eq!(
        matches!(
            queue.active.get(0).expect("incoming").track.0.status,
            PlayerTrackStatus::Paused { .. }
        ),
        auto
    );
    assert_ne!(
        queue
            .tracks
            .records_mut()
            .iter()
            .find(|record| record.id == incoming)
            .expect("incoming row")
            .status,
        TrackStatus::Cancelled
    );
    for index in [0, 1] {
        queue
            .active
            .get_mut(index)
            .expect("receiver")
            .track
            .1
            .clear();
    }
    let quiet = &mut queue.active.get_mut(0).expect("quiet receiver").track;
    quiet.0.status = PlayerTrackStatus::Loaded;
    quiet.2 = Some(1);
    let current = &mut queue.active.get_mut(1).expect("unchanged current").track;
    current.0.speed = 2.0;
    current.2 = None;
    queue.target.as_mut().expect("pending target").stale = None;
    let (mut deck, _deck_inbox) = channel(
        ChannelConfig::builder()
            .capacity(std::num::NonZeroUsize::MIN)
            .build(),
    );
    queue
        .apply_command(
            QueueCommand::ConfigureTrack(TrackSettingsChange::Speed(2.0), When::Next),
            None,
            &mut Outbox::new(&mut deck, &mut dispatcher),
        )
        .expect("one quiet Adopt and no current withdrawal need one credit");
    assert_eq!(queue.target.expect("target retained").stale, None);
    for index in [0, 1] {
        assert!(matches!(
            queue
                .active
                .get(index)
                .expect("receiver")
                .track
                .1
                .as_slice(),
            [(TrackSettingsChange::Speed(2.0), When::Next)]
        ));
    }
}

fn selected_second() -> (TestQueue, TrackId, TrackId) {
    let config = QueueConfig {
        factory: SnapshotFactory,
        downloader: None,
        mixer: DeckMixerConfig::default(),
        settings: QueueSettings::default(),
        preload_lead: Duration::from_millis(3_500),
        track: TrackSettings::default(),
        prep: None,
        cancel: None,
        store: None,
        max_concurrent_loads: std::num::NonZeroUsize::new(3).expect("default load cap"),
        runtime: None,
        should_autoplay: false,
        max_history_size: 100,
        playback_order: PlaybackOrder::default(),
        action_at_item_end: ActionAtItemEnd::None,
    };
    let mut queue = Queue::new(config);
    let first = TrackId::allocate();
    let second = TrackId::allocate();
    for (id, slot, role) in [
        (first, Slot::new(1), Role::Outgoing),
        (second, Slot::new(0), Role::Current),
    ] {
        queue.tracks.records_mut().push(TrackRecord::new(
            id,
            "repeated".to_owned(),
            TrackSource::from("https://example.com/repeated.mp3"),
            kithara_play::Position::ZERO,
        ));
        queue.tracks.set_status(id, TrackStatus::Loaded);
        let mut track = queue
            .config
            .factory
            .track(PlayerConfig {
                item: id,
                slot: Some(slot),
                settings: TrackSettings::default(),
            })
            .expect("snapshot track");
        track.0.status = PlayerTrackStatus::Playing {
            since: SessionFrame::new(0),
        };
        queue.active.push(Active {
            item: id,
            slot,
            track,
            role,
            load: None,
        });
    }
    queue.current = Some(second);
    queue.publish();
    (queue, first, second)
}

fn report(queue: &mut TestQueue, event: DeckEvent) {
    let (mut deck, _deck_inbox) = channel(ChannelConfig::builder().build());
    let (mut dispatcher, _dispatcher_inbox) = channel(ChannelConfig::builder().build());
    queue.item_event(event, None, &mut Outbox::new(&mut deck, &mut dispatcher));
    queue.publish();
}

#[kithara::test]
#[case(Role::Current)]
#[case(Role::Outgoing)]
fn natural_end_names_the_track_that_accepted_the_receipt(#[case] role: Role) {
    let (mut queue, first, second) = selected_second();
    let active = queue
        .active
        .iter()
        .find(|active| active.role == role)
        .expect("active role");
    let slot = active.slot;
    let id = active.item;
    let at = SessionFrame::new(1024);
    let mut events = queue.subscribe::<kithara_play::PlayerEvent>();
    report(&mut queue, DeckEvent::Ended { slot, at });
    assert!(
        events.try_recv().is_err(),
        "an unaccepted end must not be published"
    );
    queue
        .active
        .get_mut(if role == Role::Current { 1 } else { 0 })
        .expect("active track")
        .track
        .0
        .status = PlayerTrackStatus::Ended { at };
    report(&mut queue, DeckEvent::Ended { slot, at });
    let kithara_play::PlayerEvent::ItemDidPlayToEnd { item } =
        events.try_recv().expect("accepted natural end").event
    else {
        panic!("natural end must publish ItemDidPlayToEnd");
    };
    assert_eq!(item.id(), id);
    assert_eq!(id, if role == Role::Current { second } else { first });
    assert_eq!(item.track().slot.value(), u64::from(slot.get()));
    assert!(matches!(
        (role, item),
        (Role::Current, kithara_play::ItemRole::Leading(_))
            | (Role::Outgoing, kithara_play::ItemRole::Outgoing(_))
    ));
}

#[kithara::test]
fn an_underrun_on_the_current_track_reports_a_stall_before_its_recovery() {
    let (mut queue, _, item) = selected_second();
    let current = queue.active_current_index().expect("current track");
    let slot = queue.active.get(current).expect("current track").slot;
    let position = Duration::from_millis(730);
    queue
        .active
        .get_mut(current)
        .expect("current track")
        .track
        .0
        .position = position;
    let mut events = queue.subscribe::<ItemEvent>();
    report(
        &mut queue,
        DeckEvent::UnderrunStarted {
            slot,
            item,
            at: SessionFrame::new(128),
            position: Some(position),
        },
    );
    assert!(matches!(
        events.try_recv().expect("stall starts while PCM is missing").event,
        ItemEvent::PlaybackStalled { id, position: actual }
            if id == item && actual == position
    ));
    assert!(events.try_recv().is_err(), "recovery needs a later RT edge");
    report(
        &mut queue,
        DeckEvent::UnderrunEnded {
            slot,
            item,
            at: SessionFrame::new(192),
            position: Some(position),
            frames: 64,
            recovered: true,
        },
    );
    assert!(matches!(
        events.try_recv().expect("PCM recovery").event,
        ItemEvent::PlaybackLikelyToKeepUp { id, position: actual, frames: 64 }
            if id == item && actual == position
    ));
    assert!(events.try_recv().is_err());
}

#[kithara::test]
#[case::outgoing(Slot::new(1))]
#[case::unknown(Slot::new(2))]
#[case::reused_slot(Slot::new(0))]
fn an_underrun_start_on_a_track_that_is_not_current_reports_nothing(#[case] slot: Slot) {
    let (mut queue, first, _) = selected_second();
    let mut events = queue.subscribe::<ItemEvent>();
    report(
        &mut queue,
        DeckEvent::UnderrunStarted {
            slot,
            item: first,
            at: SessionFrame::new(128),
            position: Some(Duration::from_millis(730)),
        },
    );
    assert!(events.try_recv().is_err());
}

#[kithara::test]
#[case::outgoing(Role::Outgoing)]
#[case::leaving(Role::Leaving)]
fn an_underrun_recovery_survives_a_current_track_handover(#[case] role: Role) {
    let (mut queue, successor, item) = selected_second();
    let current = queue.active_current_index().expect("current track");
    let slot = queue.active.get(current).expect("current track").slot;
    let mut events = queue.subscribe::<ItemEvent>();
    report(
        &mut queue,
        DeckEvent::UnderrunStarted {
            slot,
            item,
            at: SessionFrame::new(128),
            position: Some(Duration::from_millis(730)),
        },
    );
    assert!(matches!(events.try_recv().expect("active stall").event,
        ItemEvent::PlaybackStalled { id, .. } if id == item));
    let previous = queue.active.get_mut(current).expect("previous current");
    previous.role = role;
    previous.track.0.position = Duration::from_secs(9);
    queue.active.get_mut(0).expect("successor").role = Role::Current;
    queue.current = Some(successor);
    report(
        &mut queue,
        DeckEvent::UnderrunEnded {
            slot,
            item,
            at: SessionFrame::new(192),
            position: Some(Duration::from_millis(740)),
            frames: 64,
            recovered: true,
        },
    );
    assert!(
        matches!(events.try_recv().expect("original item recovery").event,
        ItemEvent::PlaybackLikelyToKeepUp { id, position, frames: 64 }
            if id == item && position == Duration::from_millis(740))
    );
    assert!(events.try_recv().is_err());
}

#[kithara::test]
fn an_underrun_recovery_survives_a_detach_receipt_and_slot_reuse() {
    let (mut queue, successor, item) = selected_second();
    let slot = queue.active.get(1).expect("current").slot;
    let mut events = queue.subscribe::<ItemEvent>();
    report(
        &mut queue,
        DeckEvent::UnderrunStarted {
            slot,
            item,
            at: SessionFrame::new(128),
            position: Some(Duration::from_millis(730)),
        },
    );
    assert!(matches!(events.try_recv().expect("active stall").event,
        ItemEvent::PlaybackStalled { id, .. } if id == item));
    queue.active.get_mut(1).expect("current").role = Role::Leaving;
    let seq = fixture_seq();
    let outcome = Outcome::Applied {
        at: SessionFrame::new(192),
        data: (),
    };
    let mut batch = Batch {
        basis: vec![(slot, None)],
        commands: vec![DeckPart::Detach { slot }],
    };
    let (mut deck, _inbox) = channel(ChannelConfig::builder().build());
    let (mut dispatcher, _dispatcher_inbox) = channel(ChannelConfig::builder().build());
    queue.settle_with_output(
        TrackReceipt::Deck {
            seq,
            outcome: &outcome,
            batch: &mut batch,
        },
        None,
        &mut Outbox::new(&mut deck, &mut dispatcher),
    );
    assert!(queue.active.iter().all(|active| active.item != item));
    let replacement = queue.active.get_mut(0).expect("successor retains custody");
    replacement.slot = slot;
    replacement.track.0.slot = Some(slot);
    replacement.track.0.position = Duration::from_secs(9);
    replacement.role = Role::Current;
    queue.current = Some(successor);
    report(
        &mut queue,
        DeckEvent::UnderrunEnded {
            slot,
            item,
            at: SessionFrame::new(192),
            position: Some(Duration::from_millis(740)),
            frames: 64,
            recovered: false,
        },
    );
    assert!(
        matches!(events.try_recv().expect("retired item recovery").event,
        ItemEvent::PlaybackLikelyToKeepUp { id, position, frames: 64 }
            if id == item && position == Duration::from_millis(740))
    );
    assert!(events.try_recv().is_err());
    assert_eq!(queue.current, Some(successor));
}

#[kithara::test]
fn an_untimed_underrun_does_not_invent_a_media_position() {
    let (mut queue, _, item) = selected_second();
    let slot = queue.active.get(1).expect("current").slot;
    let mut events = queue.subscribe::<ItemEvent>();
    for event in [
        DeckEvent::UnderrunStarted {
            slot,
            item,
            at: SessionFrame::new(128),
            position: None,
        },
        DeckEvent::UnderrunEnded {
            slot,
            item,
            at: SessionFrame::new(192),
            position: None,
            frames: 64,
            recovered: false,
        },
    ] {
        report(&mut queue, event);
        assert!(events.try_recv().is_err());
    }
}

fn accepted_failure(queue: &mut TestQueue, id: TrackId, fault: PlaybackFault) -> DeckEvent {
    let active = queue
        .active
        .iter_mut()
        .find(|active| active.item == id)
        .expect("active fixture item");
    let at = SessionFrame::new(7);
    active.track.0.status = PlayerTrackStatus::Failed { at, fault };
    DeckEvent::Failed {
        slot: active.slot,
        at,
        fault,
    }
}

fn decode_fault() -> PlaybackFault {
    PlaybackFault::Source(TrackFailureKind::Decode {
        kind: DecodeErrorKind::InvalidData,
    })
}

#[kithara::test]
fn leading_failure_marks_the_played_entry_when_sources_repeat() {
    let (mut queue, first, second) = selected_second();
    let event = accepted_failure(&mut queue, second, decode_fault());
    report(&mut queue, event);
    assert!(
        !matches!(
            queue.track(first).map(|entry| entry.status),
            Some(TrackStatus::Failed(_))
        ),
        "an event for the second repeated source must not fail the first entry"
    );
    assert!(
        matches!(
            queue.track(second).map(|entry| entry.status),
            Some(TrackStatus::Failed(_))
        ),
        "the entry named by the player event must be failed"
    );
}

#[kithara::test]
#[case::invalid_data(PlaybackFault::Source(TrackFailureKind::Decode { kind: DecodeErrorKind::InvalidData }))]
#[case::unsupported_codec(PlaybackFault::Source(TrackFailureKind::Decode { kind: DecodeErrorKind::UnsupportedCodec }))]
#[case::direct_io(PlaybackFault::Source(TrackFailureKind::Decode { kind: DecodeErrorKind::Io }))]
#[case::output_rate(PlaybackFault::OutputRateMismatch)]
#[case::output_range(PlaybackFault::OutputRangeUnavailable)]
fn a_leading_failure_records_the_fault_the_player_reported(#[case] fault: PlaybackFault) {
    let (mut queue, first, second) = selected_second();
    let mut events = queue.subscribe::<QueueEvent>();
    let event = accepted_failure(&mut queue, second, fault);
    report(&mut queue, event);
    let Some(TrackStatus::Failed(reason)) = queue.track(second).map(|entry| entry.status) else {
        panic!("the entry named by the player event must be failed");
    };
    let published = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|envelope| match envelope.event {
            QueueEvent::TrackLoadFailed {
                id,
                reason,
                auto_skipped,
            } => Some((id, reason, auto_skipped)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(published, [(second, reason.clone(), false)]);
    assert_eq!(
        reason,
        fault.to_string(),
        "status and event must report the real cause without fabricating an engine failure"
    );
    assert!(
        !matches!(
            queue.track(first).map(|entry| entry.status),
            Some(TrackStatus::Failed(_))
        ),
        "a repeated URI does not make the first entry the failed item"
    );
    report(&mut queue, event);
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .all(|envelope| !matches!(envelope.event, QueueEvent::TrackLoadFailed { .. }))
    );
}

#[kithara::test]
#[case::stale(false)]
#[case::paused(true)]
fn a_stale_or_paused_failure_cannot_change_status_or_publish_a_failure(#[case] paused: bool) {
    let (mut queue, first, second) = selected_second();
    let reported = if paused { second } else { first };
    if paused {
        let active = queue
            .active
            .iter_mut()
            .find(|active| active.item == second)
            .expect("setup allocates a player slot");
        assert_eq!(active.role, Role::Current);
        active.track.0.status = PlayerTrackStatus::Paused { at: Duration::ZERO };
        assert!(
            matches!(active.track.0.status, PlayerTrackStatus::Paused { .. }),
            "setup must pause the active player"
        );
        assert_eq!(queue.current().map(|entry| entry.id), Some(second));
    }
    let before = queue
        .track(reported)
        .expect("the reported entry exists")
        .status;
    let mut events = queue.subscribe::<QueueEvent>();
    let slot = queue
        .active
        .iter()
        .find(|active| active.item == reported)
        .expect("reported player slot")
        .slot;
    report(
        &mut queue,
        DeckEvent::Failed {
            slot,
            at: SessionFrame::new(7),
            fault: decode_fault(),
        },
    );
    assert_eq!(queue.current().map(|entry| entry.id), Some(second));
    assert_eq!(
        queue.track(reported).expect("the entry survives").status,
        before
    );
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .all(|envelope| !matches!(envelope.event, QueueEvent::TrackLoadFailed { .. })),
        "an ignored item failure must not publish a queue failure"
    );
}

#[kithara::test]
fn background_end_and_failure_leave_the_current_entry_untouched() {
    let (mut queue, background, current) = selected_second();
    report(
        &mut queue,
        DeckEvent::Ended {
            slot: Slot::new(1),
            at: SessionFrame::new(7),
        },
    );
    let event = accepted_failure(&mut queue, background, decode_fault());
    report(&mut queue, event);
    assert_eq!(queue.current().map(|entry| entry.id), Some(current));
    assert!(
        !matches!(
            queue.track(background).map(|entry| entry.status),
            Some(TrackStatus::Failed(_))
        ),
        "a background failure must not fail its queue entry"
    );
}

#[kithara::test]
fn failed_playback_stopped_notification_carries_the_role() {
    let (mut queue, background, _) = selected_second();
    let event = accepted_failure(&mut queue, background, decode_fault());
    report(&mut queue, event);
    let active = queue
        .active
        .iter()
        .find(|active| active.item == background)
        .expect("outgoing player remains resident");
    assert_eq!(active.role, Role::Outgoing);
    assert!(matches!(
        active.track.snapshot().status,
        PlayerTrackStatus::Failed {
            fault: PlaybackFault::Source(TrackFailureKind::Decode {
                kind: DecodeErrorKind::InvalidData
            }),
            ..
        }
    ));
}

#[kithara::test]
#[case::remaining_equals_crossfade(162.0, 157.0, 5.0, None, true)]
#[case::remaining_below_crossfade(162.0, 160.0, 5.0, None, true)]
#[case::far_from_end(162.0, 100.0, 5.0, None, false)]
#[case::already_armed_for_same_track(162.0, 160.0, 5.0, Some(true), false)]
#[case::armed_for_different_track_still_arms(162.0, 160.0, 5.0, Some(false), true)]
#[case::crossfade_zero_at_tail_no_pre_arm(162.0, 161.9, 0.0, None, false)]
#[case::crossfade_zero_quiet_middle(162.0, 161.0, 0.0, None, false)]
#[case::zero_position_rejected(162.0, 0.0, 5.0, None, false)]
#[case::zero_duration_rejected(0.0, 10.0, 5.0, None, false)]
fn should_arm_crossfade_cases(
    #[case] duration: f64,
    #[case] position: f64,
    #[case] crossfade: f32,
    #[case] armed_for_current: Option<bool>,
    #[case] expected: bool,
) {
    let (mut queue, outgoing, current) = selected_second();
    let rate = NonZeroU32::new(10_000).expect("fixture sample rate");
    let spec = AudioSpec::new(1, rate);
    let now = SessionFrame::new(
        i64::try_from(
            spec.frames_for(Duration::from_secs_f64(position))
                .expect("fixture position")
                .get(),
        )
        .expect("fixture clock"),
    );
    queue.deck.mixer.sample_rate = rate.get();
    queue.clock = Some((now, FrameCount::new(0)));
    queue.config.action_at_item_end = ActionAtItemEnd::Advance;
    let settings = CrossfadeSettings {
        duration: crossfade,
        ..CrossfadeSettings::default()
    };
    queue.config.settings = QueueSettings::builder().crossfade(settings).build();
    let track = queue
        .active
        .iter_mut()
        .find(|active| active.item == current)
        .expect("current fixture track");
    track.track.0.position = Duration::from_secs_f64(position);
    track.track.0.duration = Some(Duration::from_secs_f64(duration));
    let incoming = TrackId::allocate();
    queue.tracks.records_mut().push(TrackRecord::new(
        incoming,
        "next".to_owned(),
        TrackSource::from("https://example.com/next.mp3"),
        kithara_play::Position::ZERO,
    ));
    let mut track = queue
        .config
        .factory
        .track(PlayerConfig {
            item: incoming,
            slot: Some(Slot::new(2)),
            settings: TrackSettings::default(),
        })
        .expect("pending fixture track");
    track.0.status = PlayerTrackStatus::Loading;
    queue.active.push(Active {
        item: incoming,
        slot: Slot::new(2),
        track,
        role: Role::Preloaded,
        load: Some(crate::queue::slots::LoadState::Opening(fixture_seq())),
    });
    queue.current = Some(current);
    if armed_for_current == Some(true) {
        queue.target = Some(crate::queue::types::Target {
            to: incoming,
            bound: Some(
                queue
                    .auto_bound(settings)
                    .expect("auto bound")
                    .expect("known duration"),
            ),
            settings,
            transition: Transition::Crossfade,
            reason: crate::AdvanceReason::NaturalEof,
            playing: true,
            auto: true,
            stale: None,
            continued: None,
            repeat: None,
            chained: false,
        });
    } else if armed_for_current == Some(false) {
        assert!(
            queue
                .active
                .iter()
                .any(|active| active.item == outgoing && active.role == Role::Outgoing)
        );
    }
    let before = queue.target.map(|target| target.to);
    let (mut deck, _deck_inbox) = channel(ChannelConfig::builder().build());
    let (mut dispatcher, _dispatcher_inbox) = channel(ChannelConfig::builder().build());
    queue
        .tick_deadlines(now, None, &mut Outbox::new(&mut deck, &mut dispatcher))
        .expect("automatic transition admission");
    assert_eq!(before.is_none() && queue.target.is_some(), expected);
    if expected {
        let target = queue.target.expect("one auto transition");
        assert_eq!(target.to, incoming);
        assert!(target.auto);
        let sent = target.bound;
        queue
            .tick_deadlines(now, None, &mut Outbox::new(&mut deck, &mut dispatcher))
            .expect("repeat deadline tick");
        assert_eq!(queue.target.expect("same transition").bound, sent);
    }
}
