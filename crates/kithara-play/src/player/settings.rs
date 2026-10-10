use kithara_config::Config;
use kithara_derive::Patch;
use kithara_events::TrackId;
use kithara_render::{LaneCommand, LaneStart, bridge::Slot};
use kithara_warp::{MIN_SPEED, SpeedCurve, StretchKind, WarpConfig};

use crate::PlayError;

/// What a track plays with that changes while it plays.
///
/// A change of one field goes to the track's render lane as one lane command;
/// the lane's receipt moves it into the settings the track's owner reads. The
/// track executes its speed itself: it alone knows where its lane stands.
#[derive(Clone, Copy, Debug, PartialEq, Config, Patch)]
#[config(default, check(error = PlayError), fields(value, get(copy)))]
pub struct TrackSettings {
    /// How fast the track plays, 1.0 at its own tempo.
    #[config(live(owner), check = check_speed, builder(default = 1.0))]
    speed: f32,
    /// Whether the pitch stays put at any speed; only a backend with keylock
    /// keeps it.
    #[config(live, builder(default = false))]
    keylock: bool,
    /// The time-stretch backend that renders the track.
    #[config(live, builder(default))]
    backend: StretchKind,
}

impl TrackSettings {
    /// The applied settings a newly opened lane starts with.
    #[must_use]
    pub fn lane_start(self) -> LaneStart {
        LaneStart {
            speed: SpeedCurve::Constant(self.speed),
            keylock: self.keylock,
            backend: self.backend,
        }
    }

    /// `base` for the renderer of a track that starts where these settings
    /// stand.
    #[must_use]
    pub fn warp(self, base: &WarpConfig) -> WarpConfig {
        base.starting_at(self.speed, self.keylock, self.backend)
    }
}

/// A speed is finite and no slower than the slowest speed the renderer plays.
pub(super) fn check_speed(speed: f32) -> Result<f32, PlayError> {
    if speed.is_finite() {
        Ok(speed.max(MIN_SPEED))
    } else {
        Err(PlayError::InvalidParameter {
            name: "speed".to_owned(),
            value: speed,
        })
    }
}

impl From<TrackSettingsChange> for LaneCommand {
    fn from(change: TrackSettingsChange) -> Self {
        match change {
            TrackSettingsChange::Speed(speed) => Self::SetSpeed(SpeedCurve::Constant(speed)),
            TrackSettingsChange::Keylock(on) => Self::SetKeylock(on),
            TrackSettingsChange::Backend(kind) => Self::SetBackend(kind),
        }
    }
}

/// What one track is built from: the item it plays, the mixer slot its deck
/// assigned it, and the settings it starts with.
#[derive(Clone, Copy, Debug, Config, Patch)]
#[config(construction)]
#[patch(fallible)]
pub struct PlayerConfig {
    #[config(value)]
    #[patch(skip)]
    pub item: TrackId,
    #[config(value)]
    #[patch(skip)]
    /// The mixer slot its deck seats it in at build; `None` for a background load
    /// that a later `TrackCommand::Seat` seats.
    pub slot: Option<Slot>,
    #[config(nested)]
    #[patch(nested, fallible)]
    pub settings: TrackSettings,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_command::{ChannelConfig, Inbox, Live, LiveError, Sender, When, channel};
    use kithara_config::ConfigOwner;
    use kithara_render::{LaneApplied, LaneCommand, LaneFrame, LaneProtocol};
    use kithara_signal::{FrameCount, SegmentId};
    use kithara_test_utils::kithara;
    use kithara_warp::{SpeedCurve, StretchKind, WarpConfig};

    use super::{PlayerConfigPatch, TrackSettings, TrackSettingsChange};
    use crate::PlayError;

    fn lane() -> (Sender<LaneProtocol>, Inbox<LaneProtocol>) {
        channel(ChannelConfig::builder().build())
    }

    fn settings() -> Live<TrackSettings, LaneProtocol> {
        Live::new(
            TrackSettings::builder()
                .speed(1.0)
                .keylock(false)
                .backend(StretchKind::default())
                .build(),
        )
        .expect("unity speed is a valid track speed")
    }

    /// Plays the lane at `frame`: applies every batch due there and returns
    /// their commands.
    fn execute(inbox: &mut Inbox<LaneProtocol>, frame: u64) -> Vec<LaneCommand> {
        inbox.drain();
        let mut commands = Vec::new();
        while let Some(due) = inbox.next_due(
            LaneFrame {
                segment: SegmentId::FIRST,
                frame,
            },
            1,
        ) {
            commands.extend(due.commands().iter().cloned());
            due.apply(LaneApplied {
                engine_latency: FrameCount::new(0),
                ready: None,
            });
        }
        commands
    }

    #[kithara::test]
    #[case::next(When::Next, 0)]
    #[case::at_frame(When::At(LaneFrame { segment: SegmentId::FIRST, frame: 4_096 }), 4_096)]
    fn a_change_shows_once_the_lane_applied_it(#[case] when: When<LaneFrame>, #[case] frame: u64) {
        let (mut sender, mut inbox) = lane();
        let mut live = settings();

        live.send(
            &mut sender,
            when,
            TrackSettingsChange::Keylock(true),
            LaneCommand::from,
        )
        .expect("the lane has room for one batch");
        assert!(
            !live.config().keylock(),
            "a sent change waits for its receipt"
        );

        let commands = execute(&mut inbox, frame);
        assert!(
            matches!(commands.as_slice(), [LaneCommand::SetKeylock(true)]),
            "the lane executes the change as its own command: {commands:?}"
        );
        for receipt in sender.receipts() {
            live.settle(&receipt);
        }
        assert!(live.config().keylock(), "the applied change shows");
    }

    #[kithara::test]
    fn every_change_reaches_the_lane_as_its_command() {
        assert!(matches!(
            LaneCommand::from(TrackSettingsChange::Speed(1.07)),
            LaneCommand::SetSpeed(SpeedCurve::Constant(speed)) if speed == 1.07
        ));
        assert!(matches!(
            LaneCommand::from(TrackSettingsChange::Keylock(true)),
            LaneCommand::SetKeylock(true)
        ));
        let backend = StretchKind::default();
        assert!(matches!(
            LaneCommand::from(TrackSettingsChange::Backend(backend)),
            LaneCommand::SetBackend(sent) if sent == backend
        ));
    }

    #[kithara::test]
    fn a_track_starts_with_live_warp_settings() {
        let quantum = NonZeroUsize::new(64).expect("fixture quantum is non-zero");
        let base = WarpConfig::builder()
            .speed(1.25)
            .render_quantum_frames(quantum)
            .build();
        let backend = StretchKind::default();
        let settings = TrackSettings::builder()
            .speed(0.8)
            .keylock(true)
            .backend(backend)
            .build();

        let warp = settings.warp(&base);

        assert!((warp.speed() - 0.8).abs() < f32::EPSILON);
        assert!(warp.keylock());
        assert_eq!(warp.backend(), backend);
        assert_eq!(warp.render_quantum_frames(), Some(quantum));
    }

    #[kithara::test]
    fn a_speed_under_the_floor_never_reaches_the_lane() {
        let (mut sender, mut inbox) = lane();
        let mut live = settings();

        let refused = live.send(
            &mut sender,
            When::Next,
            TrackSettingsChange::Speed(f32::NAN),
            LaneCommand::from,
        );

        assert!(matches!(
            refused,
            Err(LiveError::Invalid(PlayError::InvalidParameter { .. }))
        ));
        assert!(execute(&mut inbox, 0).is_empty(), "nothing was sent");
        assert!((live.config().speed() - 1.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    #[case(0.0)]
    #[case(-1.0)]
    #[case(f32::NAN)]
    fn a_rate_under_the_floor_requests_the_slowest_speed(#[case] rate: f32) {
        let (mut sender, mut inbox) = lane();
        let mut live = settings();
        let sent = live.send(
            &mut sender,
            When::Next,
            TrackSettingsChange::Speed(rate),
            LaneCommand::from,
        );
        if rate.is_nan() {
            assert!(matches!(
                sent,
                Err(LiveError::Invalid(PlayError::InvalidParameter { .. }))
            ));
            assert!(execute(&mut inbox, 0).is_empty(), "nothing was sent");
            assert_eq!(live.config().speed(), 1.0);
            assert_eq!(live.projected().speed(), 1.0);
            assert_eq!(live.pending().count(), 0);
            return;
        }
        sent.expect("finite speeds clamp to the renderer floor");

        assert!(matches!(
            execute(&mut inbox, 0).as_slice(),
            [LaneCommand::SetSpeed(SpeedCurve::Constant(speed))] if *speed == kithara_warp::MIN_SPEED
        ));
        for receipt in sender.receipts() {
            live.settle(&receipt);
        }
        assert!((live.config().speed() - kithara_warp::MIN_SPEED).abs() < f32::EPSILON);
    }

    #[kithara::test]
    #[case(0.0)]
    #[case(-1.0)]
    #[case(f32::NAN)]
    fn an_audible_member_without_a_forward_speed_is_refused(#[case] rate: f32) {
        let (mut sender, mut inbox) = lane();
        let mut live = settings();
        let result = live.send(
            &mut sender,
            When::Next,
            TrackSettingsChange::Speed(rate),
            LaneCommand::from,
        );
        if rate.is_nan() {
            assert!(matches!(
                result,
                Err(LiveError::Invalid(PlayError::InvalidParameter { .. }))
            ));
            assert!(execute(&mut inbox, 0).is_empty());
            assert_eq!(live.config().speed(), 1.0);
            assert_eq!(live.projected().speed(), 1.0);
            assert_eq!(live.pending().count(), 0);
        } else {
            result.expect("a finite sub-floor speed is admitted at MIN_SPEED");
            assert!(matches!(
                execute(&mut inbox, 0).as_slice(),
                [LaneCommand::SetSpeed(SpeedCurve::Constant(speed))] if *speed == kithara_warp::MIN_SPEED
            ));
            for receipt in sender.receipts() {
                live.settle(&receipt);
            }
            assert_eq!(live.config().speed(), kithara_warp::MIN_SPEED);
        }
    }

    #[kithara::test(native)]
    fn the_host_owned_sample_rate_field_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<PlayerConfigPatch>("sample_rate: 48000\n")
            .expect_err("a host-owned field must not be settable from a player document");
        assert!(error.to_string().contains("sample_rate"), "{error}");
    }

    #[kithara::test(native)]
    fn the_queue_owned_prefetch_field_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<PlayerConfigPatch>("prefetch_duration: 8.0\n")
            .expect_err("a queue-owned field must not be settable from a document");
        assert!(error.to_string().contains("prefetch_duration"), "{error}");
    }
}
