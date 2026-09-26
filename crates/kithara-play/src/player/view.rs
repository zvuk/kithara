use crate::bridge::PlaybackSnapshot;

/// One coherent view of a player's live playback state.
///
/// Each field preserves its own unknown state. Decorators may refine the raw
/// player position while keeping the other fields from the same snapshot.
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

impl From<PlaybackSnapshot> for PlaybackView {
    fn from(snapshot: PlaybackSnapshot) -> Self {
        Self {
            position: Some(snapshot.position()),
            duration: (snapshot.duration() > 0.0).then_some(snapshot.duration()),
            buffered: Some(snapshot.frontier().max(snapshot.cached())),
            playing: snapshot.is_playing(),
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn view_of(frontier: f64, cached: f64) -> PlaybackView {
        PlaybackView::from(PlaybackSnapshot {
            duration: 200.0,
            frontier,
            cached,
            ..PlaybackSnapshot::default()
        })
    }

    /// A fully downloaded track must report its cached span, not the sliver
    /// the decoder has produced — that span is what a host progress bar and
    /// `loadedTimeRanges` mean by "available without more network".
    #[kithara::test]
    fn buffered_covers_the_cached_span() {
        assert_eq!(view_of(4.0, 120.0).buffered, Some(120.0));
    }

    /// The frontier is a floor, not a value the cached span replaces: a
    /// reported window that falls behind the playhead makes the host pause
    /// into a buffering deadlock.
    #[kithara::test]
    fn buffered_never_falls_behind_the_decoded_frontier() {
        assert_eq!(view_of(90.0, 12.0).buffered, Some(90.0));
    }

    #[kithara::test]
    fn buffered_is_zero_when_nothing_is_available() {
        assert_eq!(view_of(0.0, 0.0).buffered, Some(0.0));
    }
}
