use kithara_audio::SeekOutcome;
use kithara_platform::{
    maybe_send::{BoxFuture, MaybeSend, MaybeSync},
    sync::Arc,
};

use crate::{
    api::SessionBeat,
    error::PlayError,
    player::{MultiPlayerCore, PlayerImpl},
};

/// Boundary used to start a player.
#[derive(Clone, Copy, Debug, Default, derive_more::From, PartialEq)]
#[non_exhaustive]
pub enum StartAt {
    /// Start on the next available render block.
    #[default]
    Immediate,
    /// Start at an exact beat of the shared session transport.
    #[from]
    SessionBeat(SessionBeat),
}

/// Playback controls shared by standalone players and player compositions.
pub trait Player: MaybeSend + MaybeSync + 'static {
    /// Current media duration in seconds, when known.
    fn duration_seconds(&self) -> Option<f64>;

    /// Pause playback.
    fn pause(&self) -> Result<(), PlayError>;

    /// Start or resume playback immediately.
    fn play(&self) -> Result<(), PlayError> {
        self.start_at(StartAt::Immediate)
    }

    /// Seek to a track-local position in seconds.
    fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError>;

    /// Start or resume playback at a typed transport boundary.
    fn start_at(&self, start: StartAt) -> Result<(), PlayError>;
}

/// Capability for relocating a composition on its shared musical timeline.
pub trait SessionSeek: Player {
    /// Prepare every affected player and commit one session relocation.
    fn seek_session(&self, target: SessionBeat) -> BoxFuture<'_, Result<(), PlayError>>;
}

impl<T> Player for Arc<T>
where
    T: Player + ?Sized,
{
    fn duration_seconds(&self) -> Option<f64> {
        T::duration_seconds(self)
    }

    fn pause(&self) -> Result<(), PlayError> {
        T::pause(self)
    }

    fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        T::seek_seconds(self, seconds)
    }

    fn start_at(&self, start: StartAt) -> Result<(), PlayError> {
        T::start_at(self, start)
    }
}

/// A player value that can be owned by a player composition.
///
/// Implementations expose their canonical [`PlayerImpl`] leaves only through
/// the collector supplied by `kithara-play`. This lets a composition acquire
/// the existing player-owned transaction locks without making the leaves
/// directly accessible to member handles.
pub trait PlayerComponent: Player {
    /// Contribute the canonical player leaves owned by this component.
    #[doc(hidden)]
    fn collect_players(&self, collector: &mut PlayerCollector<'_>);
}

impl PlayerComponent for Arc<PlayerImpl> {
    fn collect_players(&self, collector: &mut PlayerCollector<'_>) {
        collector.push(Self::clone(self));
    }
}

/// Capability passed to [`PlayerComponent`] implementations while a
/// composition is collecting its canonical player leaves.
pub struct PlayerCollector<'a> {
    composition: Option<&'a mut CollectedComposition>,
    players: &'a mut Vec<Arc<PlayerImpl>>,
}

impl<'a> PlayerCollector<'a> {
    pub(crate) fn new(
        players: &'a mut Vec<Arc<PlayerImpl>>,
        composition: Option<&'a mut CollectedComposition>,
    ) -> Self {
        Self {
            composition,
            players,
        }
    }

    pub(crate) fn push_composition(&mut self, composition: Arc<MultiPlayerCore>) {
        let Some(collected) = self.composition.as_deref_mut() else {
            return;
        };
        match collected {
            CollectedComposition::None => {
                *collected = CollectedComposition::One(composition);
            }
            CollectedComposition::One(current) if Arc::ptr_eq(current, &composition) => {}
            CollectedComposition::One(_) | CollectedComposition::Ambiguous => {
                *collected = CollectedComposition::Ambiguous;
            }
        }
    }

    delegate::delegate! {
        to self.players {
            pub(crate) fn extend(&mut self, players: impl IntoIterator<Item = Arc<PlayerImpl>>);
            /// Add one canonical player leaf to the active composition transaction.
            #[doc(hidden)]
            pub fn push(&mut self, player: Arc<PlayerImpl>);
        }
    }
}

pub(crate) enum CollectedComposition {
    None,
    One(Arc<MultiPlayerCore>),
    Ambiguous,
}
