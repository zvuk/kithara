use std::ops::Range;

use kithara_abr::AbrHandle;
use kithara_audio::{
    ServiceClass, SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome, SourceFrameRange,
};
use kithara_bufpool::PcmPool;
use kithara_decode::DecodeError;
use kithara_platform::{maybe_send::WasmSend, sync::Arc};

use super::{PlayerResource, ReadOutcome};
use crate::{
    api::{SessionBeat, Tempo, TrackBinding},
    error::PlayError,
    player::{
        node::StreamShape,
        track::elastic_renderer::{
            ElasticPreparationOutcome, ElasticPrepareError, ElasticRenderError,
            ElasticRenderOutcome, ElasticRenderer,
        },
    },
    resource::Resource,
    session::render::RenderContext,
};

struct ElasticSources {
    source: WasmSend<Resource>,
    relocation_source: WasmSend<Resource>,
}

pub(crate) struct PreparedElasticRenderer {
    renderer: ElasticRenderer,
    sources: Option<ElasticSources>,
}

impl PreparedElasticRenderer {
    fn preparing(renderer: ElasticRenderer) -> Self {
        Self {
            renderer,
            sources: None,
        }
    }

    fn with_sources(
        self,
        source: WasmSend<Resource>,
        relocation_source: Resource,
    ) -> Result<Self, ElasticPrepareError> {
        if self.sources.is_some() {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
        Ok(Self {
            renderer: self.renderer,
            sources: Some(ElasticSources {
                source,
                relocation_source: WasmSend::new(relocation_source),
            }),
        })
    }

    pub(crate) fn abr_handle(&self) -> Option<AbrHandle> {
        self.sources
            .as_ref()
            .and_then(|sources| sources.source.get().abr_handle())
    }

    fn is_ready(&self) -> bool {
        self.sources.is_some()
    }

    fn renderer_mut(&mut self) -> &mut ElasticRenderer {
        &mut self.renderer
    }

    fn begin_relocation(
        &mut self,
        binding: &TrackBinding,
        target: SessionBeat,
        tempo: Tempo,
        revision: u64,
    ) -> Result<(), ElasticPrepareError> {
        if !self.is_ready() {
            return Err(ElasticPrepareError::SourceUnavailable);
        }
        self.renderer
            .begin_relocation(binding, target, tempo, revision)
    }

    fn poll_relocation(
        &mut self,
        revision: u64,
    ) -> Result<ElasticPreparationOutcome, ElasticRenderError> {
        let Self { renderer, sources } = self;
        let sources = sources
            .as_mut()
            .ok_or(ElasticRenderError::SourceUnavailable)?;
        renderer.poll_relocation(sources.relocation_source.get_mut(), revision)
    }

    fn discard_relocation(&mut self, revision: u64) -> Result<(), ElasticRenderError> {
        if !self.is_ready() {
            return Err(ElasticRenderError::SourceUnavailable);
        }
        self.renderer.discard_relocation(revision);
        Ok(())
    }

    fn render(
        &mut self,
        binding: &TrackBinding,
        context: &RenderContext,
        range: Range<usize>,
        output: &mut [&mut [f32]],
    ) -> Result<ElasticRenderOutcome, ElasticRenderError> {
        let Self { renderer, sources } = self;
        let sources = sources
            .as_mut()
            .ok_or(ElasticRenderError::SourceUnavailable)?;
        renderer.render(
            sources.source.get_mut(),
            sources.relocation_source.get_mut(),
            binding,
            context,
            range,
            output,
        )
    }

    pub(super) fn set_service_class(&mut self, class: ServiceClass) {
        if let Some(sources) = self.sources.as_ref() {
            sources.source.get().set_service_class(class);
            sources.relocation_source.get().set_service_class(class);
        }
    }

    delegate::delegate! {
        to self.renderer {
            pub(super) fn decoded_frontier(&self) -> f64;
            pub(crate) fn validate_retarget(
                &self,
                binding: &TrackBinding,
                anchor: SessionBeat,
                tempo: Tempo,
            ) -> Result<(), ElasticPrepareError>;
            pub(crate) fn retarget(
                &mut self,
                binding: &TrackBinding,
                anchor: SessionBeat,
                tempo: Tempo,
                revision: u64,
            ) -> Result<(), ElasticPrepareError>;
        }
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

    delegate::delegate! {
        to self.resource.get_mut() {
            pub(crate) fn activate_source_audio_authoritative(
                &mut self,
            ) -> Result<bool, SourceAudioError>;
            pub(crate) fn deactivate_source_audio(&mut self) -> Result<(), SourceAudioError>;
            pub(crate) fn request_source_audio(
                &mut self,
                range: SourceFrameRange,
                look_ahead_frames: u64,
            ) -> Result<Option<SourceAudioDemand>, SourceAudioError>;
            pub(crate) fn read_source_audio(
                &mut self,
                demand: &SourceAudioDemand,
                range: SourceFrameRange,
                output: &mut [f32],
            ) -> Result<Option<SourceAudioReadOutcome>, SourceAudioError>;
            pub(crate) fn seek_source_frame(&mut self, frame: u64) -> Result<(), DecodeError>;
        }
    }

    pub(crate) fn begin_session_seek(
        &mut self,
        binding: &TrackBinding,
        target: SessionBeat,
        tempo: Tempo,
        revision: u64,
    ) -> Result<(), PlayError> {
        let renderer = self.elastic_renderer.as_mut().ok_or(PlayError::NotReady)?;
        renderer
            .begin_relocation(binding, target, tempo, revision)
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })
    }

    pub(crate) fn poll_session_seek(&mut self, revision: u64) -> Result<bool, PlayError> {
        let renderer = self.elastic_renderer.as_mut().ok_or(PlayError::NotReady)?;
        renderer
            .poll_relocation(revision)
            .map(|outcome| outcome == ElasticPreparationOutcome::Ready)
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })
    }

    pub(crate) fn cancel_session_seek(&mut self, revision: u64) -> Result<(), PlayError> {
        let renderer = self.elastic_renderer.as_mut().ok_or(PlayError::NotReady)?;
        renderer
            .discard_relocation(revision)
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })
    }

    pub(crate) fn prepare_elastic(
        &mut self,
        binding: &TrackBinding,
        anchor: SessionBeat,
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
        if let Err(error) = renderer.begin_prefetch(self, binding, anchor, tempo, revision) {
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
        relocation_source: Resource,
    ) -> Result<PreparedElasticRenderer, ElasticPrepareError> {
        let Self {
            resource,
            elastic_renderer,
            ..
        } = self;
        elastic_renderer
            .ok_or(ElasticPrepareError::SourceUnavailable)?
            .with_sources(resource, relocation_source)
    }

    pub(crate) fn install_prepared_elastic(&mut self, prepared: PreparedElasticRenderer) {
        self.elastic_renderer = Some(prepared);
    }

    pub(crate) fn read_elastic(
        &mut self,
        binding: &TrackBinding,
        context: &RenderContext,
        range: Range<usize>,
        output: &mut [&mut [f32]],
    ) -> ReadOutcome {
        let requested_frames = range.len();
        let Some(mut renderer) = self.elastic_renderer.take() else {
            return ReadOutcome::Failed;
        };
        if !renderer.is_ready() {
            self.elastic_renderer = Some(renderer);
            return ReadOutcome::Failed;
        }
        let outcome = renderer.render(binding, context, range, output);
        self.elastic_renderer = Some(renderer);
        match outcome {
            Ok(ElasticRenderOutcome::Ready { frames }) if frames == requested_frames => {
                ReadOutcome::Full { frames }
            }
            Ok(ElasticRenderOutcome::Eof) => ReadOutcome::Eof,
            Ok(ElasticRenderOutcome::Ready { .. }) | Err(_) => ReadOutcome::Failed,
        }
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
