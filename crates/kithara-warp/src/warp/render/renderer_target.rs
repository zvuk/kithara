use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_signal::{AudioSpec, SampleCount};
use kithara_stretch::{ElasticConfig, ElasticEngine, ElasticError, StretchKind, build_engine};
use tracing::warn;

use super::renderer::WarpRenderer;

#[derive(Default)]
pub(super) struct PreparedTarget {
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
        spec: AudioSpec,
        pools: &PoolRegion<S>,
    ) -> Result<ElasticConfig<S>, ElasticError> {
        ElasticConfig::builder()
            .backend(backend)
            .sample_rate(spec.sample_rate.get())
            .channels(usize::from(spec.channels.max(1)))
            .pools(pools.clone())
            .max_source_frames(Self::MAX_SOURCE_FRAMES)
            .max_output_frames(Self::MAX_OUTPUT_FRAMES)
            .build()
    }

    pub(super) fn prepare_target(
        kind: StretchKind,
        spec: AudioSpec,
        pools: &PoolRegion<S>,
        reusable_pending: Option<SampleBuffer>,
        reusable_scratch: Option<SampleBuffer>,
    ) -> PreparedTarget {
        let result = Self::config_for(kind, spec, pools)
            .and_then(build_engine)
            .and_then(|engine| {
                let channels = usize::from(spec.channels.max(1));
                let pending_samples = SampleCount::new(
                    Self::MAX_SOURCE_FRAMES
                        .checked_mul(channels)
                        .ok_or(ElasticError::SampleCountOverflow)?,
                );
                let mut pending = reusable_pending.unwrap_or_else(|| pools.get::<f32>());
                pending
                    .ensure_len(pending_samples.get())
                    .map_err(|_| ElasticError::PoolCapacity)?;
                pending.clear();
                let scratch_samples = Self::scratch_samples(engine.as_ref(), spec)?;
                let mut scratch = reusable_scratch.unwrap_or_else(|| pools.get::<f32>());
                scratch
                    .ensure_len(scratch_samples.get())
                    .map_err(|_| ElasticError::PoolCapacity)?;
                scratch.clear();
                Ok((engine, pending, scratch))
            });
        match result {
            Ok((engine, pending, scratch)) => PreparedTarget {
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

        let kind = self.controls.backend();
        let channels = usize::from(self.spec.channels.max(1));
        let entering_unity = spec == self.spec
            && (self.active || self.pending_frames(channels) > 0)
            && self.unity_passthrough(self.controls.speed());
        if entering_unity {
            self.service_scratch();
            return;
        }
        if kind != self.current_kind || spec != self.spec || self.rebuild_pending {
            self.rebuild_pending = false;
            drop(self.deferred_scratch.take());
            self.clear_render_state();
            let reusable_pending = self.pending_source.take();
            let reusable_scratch = self.scratch.take();
            drop(self.engine.take());
            let target =
                Self::prepare_target(kind, spec, &self.pools, reusable_pending, reusable_scratch);
            self.engine = target.engine;
            self.pending_source = target.pending_source;
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
        }
    }
}
