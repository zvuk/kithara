use crate::{item::AudioPlayerItem, player::AudioPlayer, types::FfiError};

#[cfg_attr(all(feature = "uniffi", not(target_arch = "wasm32")), uniffi::export)]
impl AudioPlayer {
    /// Selects `item` with an immediate cut for `FfiTransition::None`, or the
    /// configured crossfade duration for `FfiTransition::Crossfade`.
    /// The current playing or paused state is preserved.
    ///
    /// # Errors
    /// Returns [`FfiError::InvalidArgument`] for an absent item,
    /// [`FfiError::NotReady`] for an unloaded resource, or [`FfiError::Internal`]
    /// when the underlying Queue cannot select it.
    pub fn select(
        &self,
        item: &AudioPlayerItem,
        transition: crate::types::FfiTransition,
    ) -> Result<(), FfiError> {
        self.inner.select(item, transition)
    }
}
