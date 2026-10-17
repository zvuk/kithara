use kithara_bufpool::HasPool;
use kithara_command::Seq;
use kithara_events::TrackId;
use kithara_play::{Bound, CrossfadeSettings, Position, ResourceSrc, Slot};
use kithara_signal::SegmentId;

use crate::{event::AdvanceReason, track::TrackSource};

#[derive(Clone, Copy)]
pub(super) enum SeekPosition {
    Held(Position),
    Pending {
        item: TrackId,
        seq: Seq,
        to: Position,
        segment: Option<SegmentId>,
    },
}

impl SeekPosition {
    pub(super) fn held(self) -> Option<Position> {
        match self {
            Self::Held(to) => Some(to),
            Self::Pending { .. } => None,
        }
    }
}

/// One coherent view of the current track's published playback state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct PlaybackView {
    /// Seconds playable without further network access.
    pub buffered: Option<f64>,
    /// Total media duration in seconds; `None` while unknown.
    pub duration: Option<f64>,
    /// Playback position in seconds; `None` until a stable value exists.
    pub position: Option<f64>,
    /// Whether playback is active.
    pub playing: bool,
}

/// The profile a caller requests for a track switch.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Transition {
    /// No crossfade; immediate cut.
    None,
    /// Use the queue's configured crossfade.
    Crossfade,
    /// Use an explicit crossfade profile.
    CrossfadeWith { settings: CrossfadeSettings },
}

impl Transition {
    /// Resolves the requested profile against the queue's default.
    #[must_use]
    pub const fn settings(self, default: CrossfadeSettings) -> CrossfadeSettings {
        match self {
            Self::None => CrossfadeSettings {
                duration: 0.0,
                ..default
            },
            Self::Crossfade => default,
            Self::CrossfadeWith { settings } => settings,
        }
    }
}

/// The submitted entry keeps its original item when a new target replaces it.
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingHandover {
    pub(super) seq: Seq,
    pub(super) item: TrackId,
    pub(super) slot: Slot,
    pub(super) replacement: bool,
    pub(super) reason: AdvanceReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Continuation {
    Load(Seq),
    Entry(Seq),
    Pause(Seq),
    Removal {
        seq: Seq,
        handover: Seq,
        slot: Slot,
        applied: bool,
    },
}

impl Continuation {
    pub(super) fn seq(self) -> Seq {
        match self {
            Self::Load(seq) | Self::Entry(seq) | Self::Pause(seq) | Self::Removal { seq, .. } => {
                seq
            }
        }
    }
}

/// The transition the queue carries out, and why.
#[derive(Clone, Copy, Debug)]
pub(super) struct Target {
    pub(super) to: TrackId,
    pub(super) bound: Option<Bound>,
    pub(super) settings: CrossfadeSettings,
    pub(super) transition: Transition,
    pub(super) reason: AdvanceReason,
    pub(super) playing: bool,
    /// Scheduled ahead of the current track's end rather than pressed.
    pub(super) auto: bool,
    /// The withdrawn transition whose stale receipt permits recomputation.
    pub(super) stale: Option<Seq>,
    pub(super) continued: Option<Continuation>,
    pub(super) repeat: Option<Seq>,
    pub(super) chained: bool,
}

impl Target {
    pub(super) fn set_playing(&mut self, playing: bool) {
        if self.playing != playing && !matches!(self.continued, Some(Continuation::Removal { .. }))
        {
            self.continued = None;
        }
        self.playing = playing;
    }
}

/// Where a new track should land in the queue's internal `Vec`.
#[derive(Clone, Copy, Debug)]
pub(super) enum Placement {
    /// Push past the tail — used by `Queue::append`.
    Append,
    /// Insert at a caller-resolved position — used by `Queue::insert`
    /// after it looks up `after_id`.
    At(usize),
}

pub(super) fn extract_track_name<S>(source: &TrackSource<S>) -> String
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let raw = match source {
        TrackSource::Uri(s) => s.as_str(),
        TrackSource::Config(cfg) => return name_from_src(cfg.source()),
    };
    name_from_raw(raw)
}

fn name_from_src(src: &ResourceSrc) -> String {
    match src {
        ResourceSrc::Url(url) => {
            let path = url.path();
            name_from_raw(path)
        }
        ResourceSrc::Path(p) => p.file_name().map_or_else(
            || "Unknown".to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

fn name_from_raw(s: &str) -> String {
    s.rsplit('/')
        .find(|seg| !seg.is_empty())
        .unwrap_or("Unknown")
        .to_string()
}
#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn cached_position_known_nan_canonicalises_to_unknown() {
        assert!(Position::try_from_secs_f64(f64::NAN).is_err());
    }

    #[kithara::test]
    fn a_target_carries_its_transition_and_reason() {
        let bound = Some(Bound::AtOrAfter(kithara_signal::SessionFrame::new(64)));
        let target = Target {
            playing: true,
            to: TrackId(5),
            bound,
            settings: CrossfadeSettings::default(),
            transition: Transition::Crossfade,
            reason: AdvanceReason::UserSelect,
            auto: false,
            stale: None,
            continued: None,
            repeat: None,
            chained: false,
        };

        assert_eq!(target.to, TrackId(5));
        assert_eq!(target.bound, bound);
        assert_eq!(target.reason, AdvanceReason::UserSelect);
        assert!(!target.auto);
    }

    #[kithara::test]
    fn select_phase_pending_carries_captured_policy() {
        let target = Target {
            to: TrackId(5),
            bound: Some(Bound::AtOrAfter(kithara_signal::SessionFrame::new(64))),
            settings: CrossfadeSettings {
                duration: 0.0,
                ..CrossfadeSettings::default()
            },
            transition: Transition::None,
            playing: true,
            reason: AdvanceReason::UserSelect,
            auto: false,
            stale: None,
            continued: None,
            repeat: None,
            chained: false,
        };
        assert_eq!(target.to, TrackId(5));
        assert_eq!(target.settings.duration, 0.0);
        assert!(target.playing);
        assert_eq!(target.reason, AdvanceReason::UserSelect);
    }
}
