use kithara_events::TrackId;
use kithara_sync::LoadGeneration;
use kithara_warp::RenderSnapshot;

use crate::bridge::PlaybackSnapshot;

/// Render evidence for the deck's committed load.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ResidentRender {
    /// The load has no published render context yet.
    Missing,
    /// A reader for this item belongs to another load.
    Stale { bound_load: LoadGeneration },
    /// One exact callback context and presentation frontier.
    Snapshot(RenderSnapshot),
}

/// Whether the Sync executor can stage the committed load.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResidentStaging {
    /// The executor has a staging recipe for this exact load.
    Available,
    /// The executor has no staging recipe to run.
    Unavailable,
    /// The executor can stage only another load of this deck.
    DifferentLoad {
        item_id: TrackId,
        load: LoadGeneration,
    },
}

/// The load the player intends to sound, paired with its own render evidence.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ResidentLoadObservation {
    pub(crate) item_id: TrackId,
    pub(crate) load: LoadGeneration,
    pub(crate) render: ResidentRender,
    pub(crate) staging: ResidentStaging,
}

impl ResidentLoadObservation {
    /// Identity of the committed player item.
    #[must_use]
    pub const fn item_id(&self) -> TrackId {
        self.item_id
    }

    /// Generation minted when this item entered the player slot.
    #[must_use]
    pub const fn load(&self) -> LoadGeneration {
        self.load
    }

    /// Render evidence bound to this exact item and load.
    #[must_use]
    pub const fn render(&self) -> &ResidentRender {
        &self.render
    }

    /// Whether staging still targets this committed load.
    #[must_use]
    pub const fn staging(&self) -> ResidentStaging {
        self.staging
    }
}

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
