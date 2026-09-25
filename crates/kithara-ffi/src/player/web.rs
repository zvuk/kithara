use kithara::platform::sync::Arc;

use super::AudioPlayer;
use crate::{FfiQueueSettings, Inner, types::FfiError};

#[cfg_attr(feature = "uniffi-web", uniffi::export)]
impl AudioPlayer {
    /// Create a Web player attached to the initialized host.
    ///
    /// # Errors
    /// Returns a lifecycle error if the host is not ready.
    #[cfg_attr(feature = "uniffi-web", uniffi::constructor)]
    pub fn new_web() -> Result<Arc<Self>, FfiError> {
        Self::new_web_with_queue_settings(FfiQueueSettings::default())
    }

    /// Create a Web player with optional queue settings.
    ///
    /// # Errors
    /// Returns an error if the host is not ready or a setting is invalid.
    #[cfg_attr(feature = "uniffi-web", uniffi::constructor)]
    pub fn new_web_with_queue_settings(
        queue_settings: FfiQueueSettings,
    ) -> Result<Arc<Self>, FfiError> {
        crate::web::bridge::require_initialized_domain()?;
        queue_settings.validated_patch()?;
        Ok(Arc::new(Self {
            inner: Inner::new(queue_settings),
        }))
    }

    /// Submit a new default playback rate to the Web worker.
    ///
    /// # Errors
    /// Returns an error if the worker command cannot be accepted.
    pub fn set_playing_rate(&self, rate: f32) -> Result<(), FfiError> {
        self.inner.try_set_playing_rate(rate)
    }
}
