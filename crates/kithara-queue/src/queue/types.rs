use kithara_bufpool::HasPool;
use kithara_command::Seq;
use kithara_events::TrackId;
use kithara_play::{Bound, CrossfadeSettings, ResourceSrc};

use crate::{event::AdvanceReason, track::TrackSource};

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

/// The transition the queue carries out, and why.
#[derive(Clone, Copy, Debug)]
pub(super) struct Target {
    pub(super) to: TrackId,
    pub(super) bound: Bound,
    pub(super) settings: CrossfadeSettings,
    pub(super) transition: Transition,
    pub(super) reason: AdvanceReason,
    pub(super) playing: bool,
    /// Scheduled ahead of the current track's end rather than pressed: the
    /// navigation cursor moves once it applies, and it gives way when nothing
    /// sounds by the time it enters.
    pub(super) auto: bool,
    /// The withdrawn transition whose stale receipt permits recomputation.
    pub(super) stale: Option<Seq>,
    pub(super) retry: Option<Seq>,
    pub(super) repeat: Option<Seq>,
    pub(super) chained: bool,
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
        assert!(kithara_play::Position::try_from_secs_f64(f64::NAN).is_err());
    }

    #[kithara::test]
    fn a_target_carries_its_transition_and_reason() {
        let bound = Bound::AtOrAfter(kithara_signal::SessionFrame::new(64));
        let target = Target {
            playing: true,
            to: TrackId(5),
            bound,
            settings: CrossfadeSettings::default(),
            transition: Transition::Crossfade,
            reason: AdvanceReason::UserSelect,
            auto: false,
            stale: None,
            retry: None,
            repeat: None,
            chained: false,
        };

        assert_eq!(target.to, TrackId(5));
        assert_eq!(target.bound, bound);
        assert_eq!(target.reason, AdvanceReason::UserSelect);
        assert!(!target.auto);
    }
}
