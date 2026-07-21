use std::ops::Range;

use kithara_bufpool::PcmPool;
use kithara_platform::{maybe_send::WasmSend, sync::Arc};

use super::{BoundResource, PlayerResource, PlayerResourceKind, ReadOutcome};
use crate::{
    api::{SessionBeat, Tempo, TrackBinding},
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

pub(crate) struct PreparedElasticRenderer {
    renderer: ElasticRenderer,
}

pub(crate) struct PreparingElasticRenderer {
    renderer: ElasticRenderer,
}

pub(crate) enum ElasticPreparationPoll {
    Pending(PreparingElasticRenderer),
    Ready(PreparedElasticRenderer),
}

impl PreparingElasticRenderer {
    pub(crate) fn begin(
        resource: &mut Resource,
        binding: &TrackBinding,
        anchor: SessionBeat,
        tempo: Tempo,
        revision: u64,
        shape: StreamShape,
        pool: &PcmPool,
    ) -> Result<Self, ElasticPrepareError> {
        let spec = resource.spec();
        let mut renderer = ElasticRenderer::prepare(
            spec.sample_rate,
            usize::from(spec.channels),
            binding.map().source_frame_count(),
            shape,
            pool,
        )?;
        renderer.begin_prefetch(resource, binding, anchor, tempo, revision)?;
        Ok(Self { renderer })
    }

    pub(crate) fn poll(
        mut self,
        resource: &mut Resource,
    ) -> Result<ElasticPreparationPoll, ElasticPrepareError> {
        match self.renderer.poll_preparation(resource)? {
            ElasticPreparationOutcome::Pending => Ok(ElasticPreparationPoll::Pending(self)),
            ElasticPreparationOutcome::Ready => {
                Ok(ElasticPreparationPoll::Ready(PreparedElasticRenderer {
                    renderer: self.renderer,
                }))
            }
        }
    }
}

impl PreparedElasticRenderer {
    delegate::delegate! {
        to self.renderer {
            fn render(
                &mut self,
                source: &mut Resource,
                binding: &TrackBinding,
                context: &RenderContext,
                range: Range<usize>,
                output: &mut [&mut [f32]],
            ) -> Result<ElasticRenderOutcome, ElasticRenderError>;
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
    pub(crate) fn new_bound(
        resource: Resource,
        src: Arc<str>,
        renderer: PreparedElasticRenderer,
    ) -> Self {
        Self {
            src,
            kind: PlayerResourceKind::Bound(Box::new(BoundResource {
                resource: WasmSend::new(resource),
                renderer,
            })),
        }
    }

    pub(crate) fn read_elastic(
        &mut self,
        binding: &TrackBinding,
        context: &RenderContext,
        range: Range<usize>,
        output: &mut [&mut [f32]],
    ) -> ReadOutcome {
        let requested_frames = range.len();
        let PlayerResourceKind::Bound(bound) = &mut self.kind else {
            return ReadOutcome::Failed;
        };
        let outcome =
            bound
                .renderer
                .render(bound.resource.get_mut(), binding, context, range, output);
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
    use std::num::NonZeroU32;

    use kithara_bufpool::PcmPool;
    use kithara_platform::sync::Arc;

    use super::{ElasticRenderer, PlayerResource, PlayerResourceKind, PreparedElasticRenderer};
    use crate::{player::node::StreamShape, test_support::empty_resource};

    #[kithara_test_utils::kithara::test]
    fn bound_resource_has_no_linear_scratch_state() {
        let resource = empty_resource("elastic.wav");
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let renderer = ElasticRenderer::prepare(
            sample_rate,
            2,
            44_100,
            StreamShape {
                sample_rate,
                max_block_frames: NonZeroU32::new(512).expect("static block size"),
            },
            &PcmPool::default(),
        )
        .expect("elastic renderer");
        let resource = PlayerResource::new_bound(
            resource,
            Arc::from("elastic.wav"),
            PreparedElasticRenderer { renderer },
        );

        assert!(matches!(resource.kind, PlayerResourceKind::Bound(_)));
    }
}
