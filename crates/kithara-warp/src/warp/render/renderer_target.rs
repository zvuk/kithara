use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_signal::{AudioSpec, SampleCount};
use kithara_stretch::{ElasticConfig, ElasticEngine, ElasticError, StretchKind, build_engine};
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
    pub(super) fn prepare_target(
        kind: StretchKind,
        spec: AudioSpec,
        pools: &PoolRegion<S>,
        source_frame_limit: usize,
        scratch_frame_limit: usize,
        reusable: PreparedTarget,
    ) -> PreparedTarget {
        let PreparedTarget {
            activation_scratch: reusable_activation_scratch,
            engine: reusable_engine,
            pending_source: reusable_pending,
            scratch: reusable_scratch,
        } = reusable;
        drop(reusable_engine);
        let result = Self::config_for(kind, spec, pools, source_frame_limit, scratch_frame_limit)
            .and_then(build_engine)
            .and_then(|engine| {
                let channels = usize::from(spec.channels.max(1));
                let pending_samples = SampleCount::new(
                    Self::RESIDENT_SOURCE_FRAME_LIMIT
                        .checked_mul(channels)
                        .ok_or(ElasticError::SampleCountOverflow)?,
                );
                let mut pending = reusable_pending.unwrap_or_else(|| pools.get::<f32>());
                pending
                    .ensure_len(pending_samples.get())
                    .map_err(|_| ElasticError::PoolCapacity)?;
                pending.clear();
                let scratch_samples = Self::scratch_samples(spec, scratch_frame_limit)?;
                let mut scratch = reusable_scratch.unwrap_or_else(|| pools.get::<f32>());
                scratch
                    .ensure_len(scratch_samples.get())
                    .map_err(|_| ElasticError::PoolCapacity)?;
                scratch.clear();
                let activation_samples =
                    Self::scratch_samples(spec, engine.capabilities().latency().output_frames())?;
                let mut activation_scratch =
                    reusable_activation_scratch.unwrap_or_else(|| pools.get::<f32>());
                activation_scratch
                    .ensure_len(activation_samples.get())
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

    fn config_for(
        backend: StretchKind,
        spec: AudioSpec,
        pools: &PoolRegion<S>,
        source_frame_limit: usize,
        output_frame_limit: usize,
    ) -> Result<ElasticConfig<S>, ElasticError> {
        ElasticConfig::builder()
            .backend(backend)
            .sample_rate(spec.sample_rate.get())
            .channels(usize::from(spec.channels.max(1)))
            .pools(pools.clone())
            .max_source_frames(source_frame_limit)
            .max_output_frames(output_frame_limit)
            .build()
    }

    fn scratch_samples(
        spec: AudioSpec,
        scratch_frame_limit: usize,
    ) -> Result<SampleCount, ElasticError> {
        scratch_frame_limit
            .checked_mul(usize::from(spec.channels.max(1)))
            .map(SampleCount::new)
            .ok_or(ElasticError::SampleCountOverflow)
    }

    fn service_scratch(&mut self) {
        if self.scratch.is_some() {
            drop(self.deferred_scratch.take());
            return;
        }
        if self.engine.is_none() {
            drop(self.deferred_scratch.take());
            return;
        }
        let required = match Self::scratch_samples(self.spec, self.scratch_frame_limit) {
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

    /// Recycle render scratch while preserving the terminal quantum's sampled
    /// controls and prepared plan.
    #[doc(hidden)]
    pub fn prepare_terminal(&mut self) {
        drop(self.retired_engine.take());
        self.service_scratch();
    }

    /// Service backend/spec changes and deferred destruction from the
    /// scheduler shell, never from the checked render core.
    pub(super) fn service_target(&mut self, spec: AudioSpec) {
        drop(self.retired_engine.take());
        self.sync_plan();

        if spec.sample_rate != self.spec.sample_rate {
            self.applied_speed.update_sample_rate(spec.sample_rate);
        }

        let kind = self.controls.backend();
        let channels = usize::from(self.spec.channels.max(1));
        let holds_source = self.active || self.pending_frames(channels) > 0;
        if kind != self.current_kind && spec == self.spec && !self.rebuild_pending && holds_source {
            self.service_scratch();
            return;
        }
        if kind != self.current_kind || spec != self.spec || self.rebuild_pending {
            self.rebuild_pending = false;
            drop(self.deferred_scratch.take());
            self.clear_render_state();
            let reusable = PreparedTarget {
                activation_scratch: self.activation_scratch.take(),
                engine: self.engine.take(),
                pending_source: self.pending_source.take(),
                scratch: self.scratch.take(),
            };
            let target = Self::prepare_target(
                kind,
                spec,
                &self.pools,
                self.source_frame_limit,
                self.scratch_frame_limit,
                reusable,
            );
            self.activation_scratch = target.activation_scratch;
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
