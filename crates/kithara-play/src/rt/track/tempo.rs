use super::{PlayerTrack, elastic_renderer::ElasticTempoPreparationOutcome};
use crate::rt::context::RenderContext;

impl PlayerTrack {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn prepare_tempo(
        &mut self,
        current: &RenderContext,
        candidate: &RenderContext,
        revision: u64,
    ) -> Result<ElasticTempoPreparationOutcome, super::elastic_renderer::ElasticRenderError> {
        let binding = self
            .binding
            .as_ref()
            .ok_or(super::elastic_renderer::ElasticRenderError::NotPrepared)?;
        self.resource
            .prepare_elastic_tempo(binding, current, candidate, revision)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn abort_tempo(&mut self, revision: u64) {
        self.resource.abort_elastic_tempo(revision);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn apply_tempo(&mut self, revision: u64) {
        self.resource.apply_elastic_tempo(revision);
    }
}
