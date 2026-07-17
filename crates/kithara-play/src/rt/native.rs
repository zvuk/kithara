use std::{mem, ops::Range};

use kithara_abr::AbrHandle;
use kithara_audio::{
    ServiceClass, SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome, SourceFrameRange,
};
use kithara_bufpool::PcmPool;
use kithara_decode::DecodeError;
use kithara_platform::{CancelToken, maybe_send::WasmSend, sync::Arc, time::Duration};
use num_traits::cast::AsPrimitive;

use super::PlayerResource;
use crate::{
    api::{Tempo, TrackBinding},
    resource::Resource,
    rt::{
        StreamShape,
        context::RenderContext,
        track::{
            elastic_renderer::{
                ElasticPreparationOutcome, ElasticPrepareError, ElasticRenderError,
                ElasticRenderOutcome, ElasticRenderer,
            },
            elastic_source::spawn_elastic_source,
        },
    },
};

enum ElasticSourceOwnership {
    Preparing,
    Dormant(Box<WasmSend<Resource>>),
    Active { abr_handle: Option<AbrHandle> },
}

pub(crate) struct PreparedElasticRenderer {
    renderer: ElasticRenderer,
    source: ElasticSourceOwnership,
}

impl PreparedElasticRenderer {
    fn preparing(renderer: ElasticRenderer) -> Self {
        Self {
            renderer,
            source: ElasticSourceOwnership::Preparing,
        }
    }

    fn into_dormant(self, source: WasmSend<Resource>) -> Result<Self, ElasticPrepareError> {
        let Self {
            renderer,
            source: ownership,
        } = self;
        if !matches!(ownership, ElasticSourceOwnership::Preparing) {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
        Ok(Self {
            renderer,
            source: ElasticSourceOwnership::Dormant(Box::new(source)),
        })
    }

    pub(crate) fn activate(
        &mut self,
        cancel: CancelToken,
        pool: PcmPool,
    ) -> Result<(), ElasticPrepareError> {
        let source = mem::replace(&mut self.source, ElasticSourceOwnership::Preparing);
        match source {
            ElasticSourceOwnership::Dormant(mut resource) => {
                let Some(activity) = resource.get_mut().take_source_audio_activity() else {
                    self.source = ElasticSourceOwnership::Dormant(resource);
                    return Err(ElasticPrepareError::SourceUnavailable);
                };
                let abr_handle = resource.get().abr_handle();
                resource.get().set_service_class(ServiceClass::Warm);
                self.renderer.attach_source_port(spawn_elastic_source(
                    (*resource).into_inner(),
                    cancel,
                    pool,
                    activity,
                ));
                self.source = ElasticSourceOwnership::Active { abr_handle };
                Ok(())
            }
            ElasticSourceOwnership::Active { abr_handle } => {
                self.source = ElasticSourceOwnership::Active { abr_handle };
                Ok(())
            }
            ElasticSourceOwnership::Preparing => Err(ElasticPrepareError::SourceUnavailable),
        }
    }

    pub(crate) fn abr_handle(&self) -> Option<AbrHandle> {
        match &self.source {
            ElasticSourceOwnership::Dormant(resource) => resource.get().abr_handle(),
            ElasticSourceOwnership::Active { abr_handle } => abr_handle.clone(),
            ElasticSourceOwnership::Preparing => None,
        }
    }

    fn is_active(&self) -> bool {
        matches!(self.source, ElasticSourceOwnership::Active { .. })
    }

    fn renderer_mut(&mut self) -> &mut ElasticRenderer {
        &mut self.renderer
    }

    pub(super) fn decoded_frontier(&self) -> f64 {
        self.renderer.decoded_frontier()
    }

    pub(super) fn set_service_class(&mut self, class: ServiceClass) {
        self.renderer.set_service_class(class);
    }
}

impl PlayerResource {
    pub(crate) fn new_elastic(resource: Resource, src: Arc<str>) -> Self {
        Self {
            src,
            resource: WasmSend::new(resource),
            channel_buffers: None,
            eof_seen: false,
            failed: false,
            write_len: 0,
            write_pos: 0,
            elastic_renderer: None,
        }
    }

    pub(crate) fn activate_source_audio_authoritative(&mut self) -> Result<bool, SourceAudioError> {
        self.resource
            .get_mut()
            .activate_source_audio_authoritative()
    }

    pub(crate) fn deactivate_source_audio(&mut self) -> Result<(), SourceAudioError> {
        self.resource.get_mut().deactivate_source_audio()
    }

    pub(crate) fn request_source_audio(
        &mut self,
        range: SourceFrameRange,
        look_ahead_frames: u64,
    ) -> Result<Option<SourceAudioDemand>, SourceAudioError> {
        self.resource
            .get_mut()
            .request_source_audio(range, look_ahead_frames)
    }

    pub(crate) fn read_source_audio(
        &mut self,
        demand: &SourceAudioDemand,
        range: SourceFrameRange,
        output: &mut [f32],
    ) -> Result<Option<SourceAudioReadOutcome>, SourceAudioError> {
        self.resource
            .get_mut()
            .read_source_audio(demand, range, output)
    }

    pub(crate) fn seek_source_frame(&mut self, frame: u64) -> Result<(), DecodeError> {
        let sample_rate = self.resource.get().spec().sample_rate.get();
        let frame: f64 = frame.as_();
        let seconds = frame / f64::from(sample_rate);
        self.resource
            .get_mut()
            .seek(Duration::from_secs_f64(seconds))?;
        self.reset_read_state();
        Ok(())
    }

    pub(crate) fn prepare_elastic(
        &mut self,
        binding: &TrackBinding,
        tempo: Tempo,
        revision: u64,
        shape: StreamShape,
        pool: &PcmPool,
    ) -> Result<(), ElasticPrepareError> {
        let spec = self.resource.get().spec();
        let mut renderer = ElasticRenderer::prepare(
            spec.sample_rate,
            usize::from(spec.channels),
            binding.map().source_frame_count(),
            shape,
            pool,
        )?;
        if !self.activate_source_audio_authoritative()? {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
        if let Err(error) = renderer.begin_prefetch(self, binding, tempo, revision) {
            let _ = self.deactivate_source_audio();
            return Err(error);
        }
        self.elastic_renderer = Some(PreparedElasticRenderer::preparing(renderer));
        Ok(())
    }

    pub(crate) fn poll_elastic_preparation(
        &mut self,
    ) -> Result<ElasticPreparationOutcome, ElasticPrepareError> {
        let Some(mut renderer) = self.elastic_renderer.take() else {
            return Err(ElasticPrepareError::SourceUnavailable);
        };
        let outcome = renderer.renderer_mut().poll_preparation(self);
        self.elastic_renderer = Some(renderer);
        outcome
    }

    pub(crate) fn finish_elastic_preparation(
        self,
    ) -> Result<PreparedElasticRenderer, ElasticPrepareError> {
        let Self {
            resource,
            elastic_renderer,
            ..
        } = self;
        elastic_renderer
            .ok_or(ElasticPrepareError::SourceUnavailable)?
            .into_dormant(resource)
    }

    pub(crate) fn install_prepared_elastic(&mut self, prepared: PreparedElasticRenderer) {
        self.elastic_renderer = Some(prepared);
    }

    pub(crate) fn activate_prepared_elastic(
        &mut self,
        cancel: CancelToken,
        pool: PcmPool,
    ) -> Result<(), ElasticPrepareError> {
        self.elastic_renderer
            .as_mut()
            .ok_or(ElasticPrepareError::SourceUnavailable)?
            .activate(cancel, pool)
    }

    pub(crate) fn render_elastic(
        &mut self,
        binding: &TrackBinding,
        context: &RenderContext,
        range: Range<usize>,
        output: &mut [&mut [f32]],
    ) -> Result<ElasticRenderOutcome, ElasticRenderError> {
        let Some(mut renderer) = self.elastic_renderer.take() else {
            return Err(ElasticRenderError::NotPrepared);
        };
        if !renderer.is_active() {
            self.elastic_renderer = Some(renderer);
            return Err(ElasticRenderError::NotPrepared);
        }
        let outcome = renderer
            .renderer_mut()
            .render(binding, context, range, output);
        self.elastic_renderer = Some(renderer);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;

    use super::PlayerResource;
    use crate::test_support::empty_resource;

    #[kithara_test_utils::kithara::test]
    fn elastic_resource_does_not_allocate_standalone_scratch() {
        let resource =
            PlayerResource::new_elastic(empty_resource("elastic.wav"), Arc::from("elastic.wav"));

        assert!(resource.channel_buffers.is_none());
    }
}
