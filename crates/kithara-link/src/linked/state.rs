use kithara_beat::BeatGridModel;
use kithara_command::{Seq, When};
use kithara_config::Config;
use kithara_play::{Outbox, PlayError, Player, Position, Track, TrackSnapshot};
use kithara_signal::{FrameCount, SegmentId, SessionFrame};

use crate::{GridAnswer, LinkError, LinkedConfig, TempoTrajectory};

/// A player that can align itself and receive the Host's planned tempo trajectory.
pub trait LinkedPlayer<S>: Player<S> {
    /// Enables alignment, including an explicit phase jump while sounding;
    /// disabling alignment sends nothing and preserves in-flight speeds.
    ///
    /// # Errors
    /// Returns the refusal of the speed or jump batch, if one is sent.
    fn sync(&mut self, on: bool, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError>;

    /// Replaces the Host trajectory, changing sounding lanes on `at`, silent
    /// lanes on Next, and withdrawing and replanning any waiting start.
    fn retime(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>);

    /// Accepts analysis for the current item only: resumes grid waits, applies
    /// silent refinements immediately, and corrects sounding phase without a jump.
    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, S>);

    /// Whether the Host should send this player tempo changes.
    fn synced(&self) -> bool;

    /// The lane's required lead while sounding in SYNC; silent players return None.
    fn lead(&self, delivery: FrameCount) -> Option<FrameCount>;

    /// Available room in all lane queues that a retime would send to.
    fn lane_room(&self) -> usize;

    /// Scope batches needed to reopen silent tracks during a retime.
    fn scope_parts(&self) -> usize;

    /// Whether the current native representation of this retime has applied.
    /// None awaits its current receipt.
    fn retime_applied(&mut self, at: SessionFrame) -> Option<bool>;

    /// Replaces the trajectory and closes phase using only bounded speed steps.
    fn realign(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>);
}

/// Maximum adjacent speed step used for inaudible phase correction.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, check(error = LinkError), fields(value, get(copy)))]
pub struct LinkConfig {
    #[config(check = check_epsilon, builder(default = 0.001))]
    epsilon: f32,
}

fn check_epsilon(epsilon: f32) -> Result<f32, LinkError> {
    if epsilon.is_finite() && epsilon > 0.0 {
        Ok(epsilon)
    } else {
        Err(LinkError::Epsilon { epsilon })
    }
}

/// Whether Host synchronization owns speed and phase for this track.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncMode {
    Off,
    On,
    /// Analysis refused; playback continues without synchronization.
    Unsyncable,
}

/// Synchronization progress beside the underlying track snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncStatus {
    Off,
    On,
    WaitingForGrid { required: Position },
    Unsyncable,
    Correcting { remaining: FrameCount },
}

/// An owner command waiting for a grid that covers its media position.
#[derive(Clone, Copy, Debug)]
pub enum Waiting {
    Load {
        required: Position,
    },
    Play {
        required: Position,
        at: When<SessionFrame>,
    },
    Sync {
        required: Position,
    },
    Seek {
        required: Position,
    },
}

impl Waiting {
    pub(super) fn required(self) -> Position {
        match self {
            Self::Load { required }
            | Self::Play { required, .. }
            | Self::Sync { required }
            | Self::Seek { required } => required,
        }
    }
}

/// Track state and its synchronization admission or correction state.
#[derive(Clone, Debug)]
pub struct LinkedSnapshot<T> {
    pub track: T,
    pub sync: SyncStatus,
}

impl<T: AsRef<TrackSnapshot>> AsRef<TrackSnapshot> for LinkedSnapshot<T> {
    fn as_ref(&self) -> &TrackSnapshot {
        self.track.as_ref()
    }
}

/// One track decorated with analysis, Host tempo and owner-thread synchronization.
pub struct Linked<P> {
    pub(super) inner: P,
    pub(super) config: LinkConfig,
    pub(super) grid: Option<BeatGridModel>,
    pub(super) host: TempoTrajectory,
    pub(crate) mode: SyncMode,
    pub(super) waiting: Option<Waiting>,
    pub(super) correction: Option<(Seq, RunningCorrection)>,
    pub(super) alignment: Option<bool>,
    pub(super) speeds: Vec<(Seq, SpeedOperation)>,
    pub(super) start: Option<Seq>,
    pub(super) start_caller: Option<Seq>,
    pub(super) withdrawn: Vec<Seq>,
    pub(super) retimes: Vec<(SessionFrame, Seq)>,
    pub(super) cue: Option<Cue>,
}

/// A silent entry waiting for its grid, and how the grid places it.
#[derive(Clone, Copy)]
pub(super) enum Cue {
    /// A position the grid covers stays; one before the grid moves to the
    /// first beat at or after it.
    Covered(Position),
    /// The position moves to the first beat at or after it.
    Beat(Position),
}

#[derive(Clone, Copy)]
pub(super) struct RunningCorrection {
    pub(super) at: SessionFrame,
    pub(super) frames: FrameCount,
    pub(super) segment: SegmentId,
}

pub(super) enum CorrectionState {
    Pending(RunningCorrection),
    Applied(RunningCorrection),
}

pub(super) enum SpeedOperation {
    Correction {
        commanded: bool,
        state: CorrectionState,
    },
    Alignment,
    Relocation {
        to: Position,
    },
}

impl<P> Linked<P> {
    /// Decorates a track with synchronization initially off.
    #[must_use]
    pub fn new(config: LinkedConfig<P>) -> Self {
        let LinkedConfig {
            inner,
            settings: config,
            trajectory: host,
        } = config;
        Self {
            inner,
            config,
            grid: None,
            host,
            mode: SyncMode::Off,
            waiting: None,
            correction: None,
            alignment: None,
            speeds: Vec::new(),
            start: None,
            start_caller: None,
            withdrawn: Vec::new(),
            retimes: Vec::new(),
            cue: None,
        }
    }

    pub(super) fn update_correction<S>(&mut self)
    where
        P: Track<S>,
    {
        let snapshot = self.inner.snapshot();
        let Some(mark) = snapshot.as_ref().mark else {
            return;
        };
        let eligible = self
            .speeds
            .iter()
            .filter_map(|(seq, operation)| {
                let SpeedOperation::Correction {
                    state: CorrectionState::Applied(correction),
                    ..
                } = operation
                else {
                    return None;
                };
                (correction.segment == mark.lane.segment
                    && mark.session >= correction.at
                    && self.correction.as_ref().is_none_or(|(active_seq, active)| {
                        (correction.at, *seq) > (active.at, *active_seq)
                    }))
                .then_some((*seq, *correction))
            })
            .max_by_key(|(seq, correction)| (correction.at, *seq));
        if let Some((seq, correction)) = eligible {
            self.correction = Some((seq, correction));
        }
        let active = self.correction;
        self.speeds.retain(|(seq, operation)| {
            let SpeedOperation::Correction { state, .. } = operation else {
                return true;
            };
            let (CorrectionState::Pending(correction) | CorrectionState::Applied(correction)) =
                state;
            correction.segment > mark.lane.segment
                || correction.segment == mark.lane.segment
                    && active.as_ref().is_none_or(|(active_seq, active)| {
                        (correction.at, *seq) > (active.at, *active_seq)
                    })
        });
        if let Some((_, correction)) = &self.correction
            && (mark.lane.segment != correction.segment
                || mark
                    .session
                    .frames_since(correction.at)
                    .is_some_and(|elapsed| elapsed >= correction.frames.get() as u64))
        {
            self.correction = None;
        }
    }
}
