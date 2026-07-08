use std::sync::Arc;

use crate::{Resampler, ResamplerBuildError, ResamplerCapabilities, ResamplerSettings};

pub trait ResamplerBackend: Send + Sync {
    /// Build a standalone resampler for the supplied settings.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] when the settings are invalid for this
    /// backend or backend construction fails.
    fn build(
        &self,
        settings: &ResamplerSettings,
    ) -> Result<Box<dyn Resampler>, ResamplerBuildError>;

    fn capabilities(&self) -> ResamplerCapabilities;

    fn name(&self) -> &'static str;
}

impl<T> ResamplerBackend for Box<T>
where
    T: ResamplerBackend + ?Sized,
{
    fn build(
        &self,
        settings: &ResamplerSettings,
    ) -> Result<Box<dyn Resampler>, ResamplerBuildError> {
        self.as_ref().build(settings)
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        self.as_ref().capabilities()
    }

    fn name(&self) -> &'static str {
        self.as_ref().name()
    }
}

impl<T> ResamplerBackend for Arc<T>
where
    T: ResamplerBackend + ?Sized,
{
    fn build(
        &self,
        settings: &ResamplerSettings,
    ) -> Result<Box<dyn Resampler>, ResamplerBuildError> {
        self.as_ref().build(settings)
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        self.as_ref().capabilities()
    }

    fn name(&self) -> &'static str {
        self.as_ref().name()
    }
}

impl<T> ResamplerBackend for &T
where
    T: ResamplerBackend + ?Sized,
{
    fn build(
        &self,
        settings: &ResamplerSettings,
    ) -> Result<Box<dyn Resampler>, ResamplerBuildError> {
        (*self).build(settings)
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        (*self).capabilities()
    }

    fn name(&self) -> &'static str {
        (*self).name()
    }
}
