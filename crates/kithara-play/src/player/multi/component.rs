use kithara_platform::sync::Arc;

use super::{core::MultiPlayerCore, group::MultiPlayer};
use crate::{api::PlayerComponent, player::PlayerImpl};

/// Type-erased ownership boundary for values accepted by
/// [`MultiPlayer::register`].
///
/// Consumer crates implement `From<T>` for this type through the erased
/// [`PlayerComponent`] conversion. This keeps registration type-driven
/// without teaching `kithara-play` concrete consumer types.
pub struct PlayerComponentBox {
    pub(super) component: Arc<dyn PlayerComponent>,
    pub(super) nested: Option<Arc<MultiPlayerCore>>,
    pub(super) players: Arc<[Arc<PlayerImpl>]>,
}

impl From<Box<dyn PlayerComponent>> for PlayerComponentBox {
    fn from(component: Box<dyn PlayerComponent>) -> Self {
        Self {
            component: Arc::from(component),
            nested: None,
            players: Arc::from([]),
        }
    }
}

impl PlayerComponentBox {
    pub(super) fn clone_for_dispatch(&self) -> Self {
        Self {
            component: Arc::clone(&self.component),
            nested: self.nested.as_ref().map(Arc::clone),
            players: Arc::clone(&self.players),
        }
    }
}

impl From<PlayerImpl> for PlayerComponentBox {
    fn from(player: PlayerImpl) -> Self {
        Self {
            component: Arc::new(Arc::new(player)),
            nested: None,
            players: Arc::from([]),
        }
    }
}

#[cfg(any(test, feature = "probe"))]
impl From<Arc<PlayerImpl>> for PlayerComponentBox {
    fn from(player: Arc<PlayerImpl>) -> Self {
        Self {
            component: Arc::new(player),
            nested: None,
            players: Arc::from([]),
        }
    }
}

impl From<MultiPlayer> for PlayerComponentBox {
    fn from(player: MultiPlayer) -> Self {
        Self {
            component: Arc::new(player),
            nested: None,
            players: Arc::from([]),
        }
    }
}
