use super::AudioPlayer;
use crate::types::{FfiActionAtItemEnd, FfiCrossfadeSettings, FfiError, FfiPlaybackOrder};

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl AudioPlayer {
    /// Advance to the next item, or do nothing at queue exhaustion.
    ///
    /// # Errors
    /// Returns an error when queue navigation fails.
    pub fn advance_to_next_item(&self) -> Result<(), FfiError> {
        self.inner.advance_to_next_item()
    }

    /// Return to the previous item in navigation history.
    ///
    /// # Errors
    /// Returns an error when queue navigation fails.
    pub fn return_to_previous_item(&self) -> Result<(), FfiError> {
        self.inner.return_to_previous_item()
    }
}

#[cfg_attr(any(feature = "uniffi", feature = "uniffi-web"), uniffi::export)]
impl AudioPlayer {
    /// Set the queue traversal order.
    ///
    /// # Errors
    /// Returns an error for an unknown external enum value.
    pub fn set_playback_order(&self, order: FfiPlaybackOrder) -> Result<(), FfiError> {
        self.inner.set_playback_order(order)
    }

    /// Set the automatic terminal action.
    ///
    /// # Errors
    /// Returns an error for an unknown external enum value.
    pub fn set_action_at_item_end(&self, action: FfiActionAtItemEnd) -> Result<(), FfiError> {
        self.inner.set_action_at_item_end(action)
    }

    /// Set the profile captured by future transitions.
    ///
    /// # Errors
    /// Returns an error when the profile contains an invalid value.
    pub fn set_crossfade_settings(&self, settings: FfiCrossfadeSettings) -> Result<(), FfiError> {
        self.inner.set_crossfade_settings(settings)
    }
}
