//! Free items shared by the `Queue` impl modules: `Transition`,
//! private state shapes, and pure helpers.

use kithara_events::TrackId;
use kithara_play::ResourceSrc;

use crate::track::TrackSource;

/// Transition style for a track switch.
///
/// Mirrors the Apple-idiomatic pattern of a namespace-style type with
/// variants describing "what" — not "how" — so the same method
/// signature handles both manual and auto-advance cases.
///
/// - [`Transition::None`] — immediate cut (0 seconds). Matches
///   `AVQueuePlayer`'s user-initiated selection idiom.
/// - [`Transition::Crossfade`] — use the player's configured
///   [`PlayerImpl::crossfade_duration`](kithara_play::PlayerImpl::crossfade_duration).
/// - [`Transition::CrossfadeWith`] — explicit override in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Transition {
    /// No crossfade; immediate cut.
    None,
    /// Use the player's configured crossfade duration.
    Crossfade,
    /// Use an explicit crossfade duration (seconds).
    CrossfadeWith { seconds: f32 },
}

impl Transition {
    /// Resolve the transition to an actual crossfade duration in
    /// seconds using `default` for [`Transition::Crossfade`].
    #[must_use]
    pub fn crossfade_seconds(self, default: f32) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Crossfade => default,
            Self::CrossfadeWith { seconds } => seconds,
        }
    }
}

/// A pending-select entry: a track id waiting to be applied plus the
/// [`Transition`] the caller asked for. Stored until loading finishes.
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingSelect {
    pub(super) id: TrackId,
    pub(super) transition: Transition,
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

/// Decide whether `Queue::tick` should arm the pre-end advance.
///
/// Returns `true` when:
/// - `pos` and `dur` are positive (track has meaningful position + duration), AND
/// - remaining playtime is below the arm threshold — either
///   `crossfade` seconds (with crossfade > 0) or `END_PROXIMITY_SECONDS`
///   (no crossfade, trigger right at the tail), AND
/// - we haven't already armed for this track this play-through.
pub(crate) fn should_arm_crossfade(
    pos: f64,
    dur: f64,
    crossfade: f32,
    current_id: TrackId,
    armed_for: Option<TrackId>,
) -> bool {
    /// How close to the end we arm the next-track advance when there is
    /// no crossfade configured — gives the queue a brief window to select
    /// and start the next track before the current one goes silent.
    const END_PROXIMITY_SECONDS: f64 = 0.25;

    if dur <= 0.0 || pos <= 0.0 {
        return false;
    }
    let threshold = if crossfade > 0.0 {
        f64::from(crossfade)
    } else {
        END_PROXIMITY_SECONDS
    };
    if dur - pos > threshold {
        return false;
    }
    armed_for != Some(current_id)
}

pub(super) fn extract_track_name(source: &TrackSource) -> String {
    let raw = match source {
        TrackSource::Uri(s) => s.as_str(),
        TrackSource::Config(cfg) => return name_from_src(&cfg.src),
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
