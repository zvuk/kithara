use std::num::NonZeroU32;

use kithara_audio::{DecodeErrorKind, TrackFailureKind};
use kithara_command::{Batch, ChannelConfig, Seq, When, channel};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_play::{
    Bound, DeckEvent, DeckPart, Outbox, PlayError, PlaybackFault, Player, PlayerConfig, Settled,
    Slot, Track, TrackCommand, TrackFactory, TrackReceipt, TrackSettings, TrackSettingsChange,
    TrackSnapshot, TrackStatus as PlayerTrackStatus,
};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::kithara;

use super::*;
use crate::{
    CrossfadeSettings, ItemEvent, PlaybackOrder, QueueConfig, QueueSettings, TrackSource,
    queue::slots::Active, test_pools::TestPools, track::TrackRecord,
};

struct SnapshotTrack(
    TrackSnapshot,
    Vec<(TrackSettingsChange, When<SessionFrame>)>,
);
struct SnapshotFactory;

impl TrackFactory<TestPools> for SnapshotFactory {
    type Track = SnapshotTrack;
    fn track(&self, config: PlayerConfig) -> Result<Self::Track, PlayError> {
        Ok(SnapshotTrack(
            TrackSnapshot {
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
        _receipt: TrackReceipt<'_, TestPools>,
        _out: &mut Outbox<'_, TestPools>,
    ) -> Settled {
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
    ) -> Result<(), PlayError> {
        if self.0.lane_room == 0 {
            return Err(PlayError::Full("lane"));
        }
        if matches!(at, When::At(_)) && !matches!(self.0.status, PlayerTrackStatus::Playing { .. })
        {
            return Err(PlayError::Untimed);
        }
        Ok(())
    }

    fn projected(&self) -> TrackSettings {
        TrackSettings::default()
    }

    fn planned(
        &self,
        _at: SessionFrame,
        _sample_rate: NonZeroU32,
    ) -> Result<(Duration, f32), PlayError> {
        panic!("terminal-event fixture does not project a render lane")
    }

    fn planned_end(&self, _sample_rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError> {
        panic!("terminal-event fixture does not project a render lane")
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
        _speed: f32,
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
}

#[kithara::test]
#[case(true)]
#[case(false)]
fn a_speed_change_on_the_current_track_withdraws_a_sent_automatic_transition(#[case] auto: bool) {
    let (mut queue, incoming, _) = selected_second();
    let batch = fixture_seq();
    let receiver = queue.active.get_mut(0).expect("incoming");
    receiver.role = Role::Incoming { batch: Some(batch) };
    receiver.track.0.status = PlayerTrackStatus::Loaded;
    receiver.track.0.lane_room = 4;
    queue.active.get_mut(1).expect("current").track.0.lane_room = 4;
    queue.target = Some(super::super::super::types::Target {
        to: incoming,
        bound: Bound::AtOrBefore(SessionFrame::new(512)),
        settings: CrossfadeSettings::default(),
        transition: super::super::super::Transition::None,
        reason: crate::AdvanceReason::NaturalEof,
        playing: true,
        auto,
        stale: None,
        retry: None,
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
}

fn selected_second() -> (TestQueue, TrackId, TrackId) {
    let config = QueueConfig {
        factory: SnapshotFactory,
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
fn an_underrun_on_the_current_track_reports_a_stall_and_its_recovery() {
    let (mut queue, _, _) = selected_second();
    let slot = queue
        .active
        .iter()
        .find(|active| active.role == Role::Current)
        .expect("current track")
        .slot;
    let mut events = queue.subscribe::<ItemEvent>();
    report(
        &mut queue,
        DeckEvent::Underrun {
            slot,
            at: SessionFrame::new(128),
            frames: 64,
        },
    );
    let published = std::iter::from_fn(|| events.try_recv().ok())
        .map(|envelope| envelope.event)
        .collect::<Vec<_>>();
    assert!(matches!(
        published.as_slice(),
        [
            ItemEvent::PlaybackStalled,
            ItemEvent::PlaybackLikelyToKeepUp
        ]
    ));
}

#[kithara::test]
#[case::outgoing(Slot::new(1))]
#[case::unknown(Slot::new(2))]
fn an_underrun_on_a_track_that_is_not_current_reports_nothing(#[case] slot: Slot) {
    let (mut queue, _, _) = selected_second();
    let mut events = queue.subscribe::<ItemEvent>();
    report(
        &mut queue,
        DeckEvent::Underrun {
            slot,
            at: SessionFrame::new(128),
            frames: 64,
        },
    );
    assert!(matches!(
        events.try_recv(),
        Err(kithara_platform::tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
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
