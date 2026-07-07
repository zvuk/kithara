use kithara_assets::StoreOptions;
use kithara_events::EventBus;
use kithara_platform::CancelToken;

use super::{ResourceConfig, ResourceSrc};

impl ResourceConfig {
    /// Event bus attached to this resource, when one was configured.
    #[must_use]
    pub fn bus(&self) -> Option<&EventBus> {
        self.bus.as_ref()
    }

    /// Optional cache-disambiguating resource name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Preferred peak bitrate cap for normal networks.
    #[must_use]
    pub fn preferred_peak_bitrate(&self) -> f64 {
        self.preferred_peak_bitrate
    }

    /// Replace the parent cancel token for this resource.
    pub fn set_cancel(&mut self, cancel: CancelToken) {
        self.cancel = Some(cancel);
    }

    /// Source parsed for this resource.
    #[must_use]
    pub fn source(&self) -> &ResourceSrc {
        &self.src
    }

    /// Storage configuration for this resource.
    #[must_use]
    pub fn store(&self) -> &StoreOptions {
        &self.store
    }

    /// Mutable storage configuration for caller-owned resource templates.
    #[must_use]
    pub fn store_mut(&mut self) -> &mut StoreOptions {
        &mut self.store
    }
}
