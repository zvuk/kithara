use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_signal::{AudioSpec, SampleCount};
use kithara_stretch::{
    ElasticBackendConfig, ElasticConfig, ElasticEngine, ElasticError, StretchKind, build_engine,
};
use tracing::warn;

use super::renderer::WarpRenderer;

#[derive(Default)]
pub(super) struct PreparedTarget {
    pub(super) activation_scratch: Option<SampleBuffer>,
    pub(super) engine: Option<Box<dyn ElasticEngine>>,
    pub(super) pending_source: Option<SampleBuffer>,
    pub(super) scratch: Option<SampleBuffer>,
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    fn config_for(
        backend: StretchKind,
        backends: ElasticBackendConfig,
        source_block_frames: NonZeroUsize,
        spec: AudioSpec,
        pools: &PoolRegion<S>,
    ) -> Result<ElasticConfig<S>, ElasticError> {
        ElasticConfig::builder()
            .backend(backend)
            .backends(backends)
            .sample_rate(spec.sample_rate.get())
            .channels(usize::from(spec.channels.max(1)))
            .pools(pools.clone())
            .max_source_frames(source_block_frames.get())
            .max_output_frames(Self::MAX_OUTPUT_FRAMES)
            .build()
    }

    pub(super) fn prepare_target(
        kind: StretchKind,
        backends: ElasticBackendConfig,
        source_block_frames: NonZeroUsize,
        spec: AudioSpec,
        pools: &PoolRegion<S>,
        reusable: PreparedTarget,
    ) -> PreparedTarget {
        let PreparedTarget {
            activation_scratch: reusable_activation_scratch,
            engine: reusable_engine,
            pending_source: reusable_pending,
            scratch: reusable_scratch,
        } = reusable;
        drop(reusable_engine);
        let result = Self::config_for(kind, backends, source_block_frames, spec, pools)
            .and_then(build_engine)
            .and_then(|engine| {
                let channels = usize::from(spec.channels.max(1));
                let pending_samples = SampleCount::new(
                    source_block_frames
                        .get()
                        .max(engine.capabilities().latency().source_frames())
                        .checked_mul(channels)
                        .ok_or(ElasticError::SampleCountOverflow)?,
                );
                let mut pending = reusable_pending.unwrap_or_else(|| pools.get::<f32>());
                pending
                    .ensure_len(pending_samples.get())
                    .map_err(|_| ElasticError::PoolCapacity)?;
                let history_samples = engine
                    .capabilities()
                    .latency()
                    .source_frames()
                    .checked_mul(channels)
                    .ok_or(ElasticError::SampleCountOverflow)?;
                pending.truncate(history_samples);
                pending.fill(0.0);
                let scratch_samples = Self::scratch_samples(engine.as_ref(), spec)?;
                let mut scratch = reusable_scratch.unwrap_or_else(|| pools.get::<f32>());
                scratch
                    .ensure_len(scratch_samples.get())
                    .map_err(|_| ElasticError::PoolCapacity)?;
                scratch.clear();
                let activation_samples = engine
                    .capabilities()
                    .latency()
                    .output_frames()
                    .checked_mul(channels)
                    .ok_or(ElasticError::SampleCountOverflow)?;
                let mut activation_scratch =
                    reusable_activation_scratch.unwrap_or_else(|| pools.get::<f32>());
                activation_scratch
                    .ensure_len(activation_samples)
                    .map_err(|_| ElasticError::PoolCapacity)?;
                activation_scratch.clear();
                Ok((engine, pending, scratch, activation_scratch))
            });
        match result {
            Ok((engine, pending, scratch, activation_scratch)) => PreparedTarget {
                activation_scratch: Some(activation_scratch),
                engine: Some(engine),
                pending_source: Some(pending),
                scratch: Some(scratch),
            },
            Err(error) => {
                warn!(%kind, %error, "time-stretch engine preparation failed");
                PreparedTarget::default()
            }
        }
    }

    fn scratch_samples(
        engine: &dyn ElasticEngine,
        spec: AudioSpec,
    ) -> Result<SampleCount, ElasticError> {
        let capabilities = engine.capabilities();
        capabilities
            .max_output_frames()
            .checked_mul(usize::from(spec.channels.max(1)))
            .map(SampleCount::new)
            .ok_or(ElasticError::SampleCountOverflow)
    }

    fn service_scratch(&mut self) {
        if self.scratch.is_some() {
            drop(self.deferred_scratch.take());
            return;
        }
        let Some(engine) = self.engine.as_deref() else {
            drop(self.deferred_scratch.take());
            return;
        };
        let required = match Self::scratch_samples(engine, self.spec) {
            Ok(required) => required,
            Err(error) => {
                warn!(%error, "time-stretch output scratch sizing failed");
                drop(self.deferred_scratch.take());
                return;
            }
        };
        let mut scratch = self
            .deferred_scratch
            .take()
            .unwrap_or_else(|| self.pools.get::<f32>());
        if scratch.ensure_len(required.get()).is_err() {
            warn!("pool capacity exhausted while preparing time-stretch output scratch");
            return;
        }
        scratch.clear();
        self.scratch = Some(scratch);
    }

    /// Service backend/spec changes and deferred destruction from the
    /// scheduler shell, never from the checked render core.
    pub(super) fn service_target(&mut self, spec: AudioSpec) {
        drop(self.retired_engine.take());
        if self.transition_pending() && spec == self.spec {
            self.service_scratch();
            return;
        }
        self.sync_plan();

        self.select_context(self.rendered_source_end.map_or(0, |(frame, _)| frame));

        let kind = self.controls.backend();
        let channels = usize::from(self.spec.channels.max(1));
        let entering_unity = spec == self.spec
            && (self.active || self.pending_frames(channels) > 0)
            && self.unity_passthrough(self.rate.speed());
        if entering_unity {
            self.service_scratch();
            return;
        }
        if kind != self.current_kind || spec != self.spec || self.rebuild_pending {
            self.rebuild_pending = false;
            drop(self.deferred_scratch.take());
            self.clear_render_state();
            let target = Self::prepare_target(
                kind,
                self.backends,
                self.source_block_frames,
                spec,
                &self.pools,
                PreparedTarget {
                    activation_scratch: self.activation_scratch.take(),
                    engine: self.engine.take(),
                    pending_source: self.pending_source.take(),
                    scratch: self.scratch.take(),
                },
            );
            self.activation_scratch = target.activation_scratch;
            self.engine = target.engine;
            self.pending_source = target.pending_source;
            self.passthrough_history_head = self.engine.is_some().then_some(0);
            self.scratch = target.scratch;
            self.current_kind = kind;
            self.spec = spec;
            self.reset_pending = false;
            return;
        }

        self.service_scratch();

        if !self.reset_pending {
            return;
        }
        self.reset_pending = false;
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = engine.reset()
        {
            warn!(%error, "time-stretch deferred reset failed");
            self.engine = None;
            self.rebuild_pending = true;
            return;
        }
        if let Err(error) = self.reset_passthrough_history() {
            warn!(%error, "time-stretch history preparation failed");
            self.clear_pending_source();
        }
    }
}
