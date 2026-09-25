use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_signal::{AudioSpec, SampleCount};
use kithara_stretch::{
    BackendCapabilities, ElasticBackendConfig, ElasticConfig, ElasticEngine, ElasticError,
    StretchKind, build_engine, build_varispeed_engine,
};
use num_traits::ToPrimitive;
use tracing::warn;

use super::{renderer::WarpRenderer, renderer_residency::SourceResidency};

#[derive(Default)]
pub(super) struct PreparedTarget {
    pub(super) activation_scratch: Option<SampleBuffer>,
    pub(super) engine: Option<Box<dyn ElasticEngine>>,
    pub(super) pending_source: Option<SampleBuffer>,
    pub(super) residency: Option<SourceResidency>,
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
        (kind, keylock): (StretchKind, bool),
        backends: ElasticBackendConfig,
        source_block_frames: NonZeroUsize,
        spec: AudioSpec,
        pools: &PoolRegion<S>,
        reusable: PreparedTarget,
        measure_capabilities: bool,
    ) -> Result<PreparedTarget, ElasticError> {
        if keylock && !kind.capabilities().contains(BackendCapabilities::KEYLOCK) {
            return Err(ElasticError::EnginePreparation(
                "selected backend does not support keylock",
            ));
        }
        let PreparedTarget {
            residency: reusable_residency,
            activation_scratch: reusable_activation_scratch,
            engine: reusable_engine,
            pending_source: reusable_pending,
            scratch: reusable_scratch,
        } = reusable;
        drop(reusable_engine);
        let channels = usize::from(spec.channels.max(1));
        let mut history_frames = reusable_residency
            .as_ref()
            .map_or(0, |resident| resident.history_frames);
        let mut resident_frames = reusable_residency.as_ref().map_or_else(
            || source_block_frames.get(),
            |resident| resident.samples.capacity() / channels,
        );
        let mut replacement_frames = reusable_residency
            .as_ref()
            .map_or(0, |resident| resident.replacement.capacity() / channels);
        let measured = (|| -> Result<(), ElasticError> {
            for backend in StretchKind::all().iter().filter(|_| measure_capabilities) {
                let capabilities =
                    Self::config_for(*backend, backends, source_block_frames, spec, pools)
                        .and_then(build_engine)
                        .map(|engine| engine.capabilities())?;
                {
                    let latency = capabilities.latency();
                    history_frames = history_frames.max(latency.source_frames());
                    let source_tail = (latency.source_frames().to_f64().unwrap_or(f64::MAX)
                        / capabilities.rate_envelope().min_source_frames_per_output())
                    .ceil()
                    .to_usize()
                    .unwrap_or(usize::MAX);
                    replacement_frames =
                        replacement_frames.max(latency.output_frames().saturating_add(source_tail));
                    let warm = (latency.output_frames().to_f64().unwrap_or(f64::MAX)
                        * capabilities.rate_envelope().max_source_frames_per_output())
                    .ceil()
                    .to_usize()
                    .unwrap_or(usize::MAX);
                    resident_frames = resident_frames.max(
                        latency
                            .source_frames()
                            .saturating_mul(2)
                            .saturating_add(warm)
                            .saturating_add(source_block_frames.get().saturating_mul(2)),
                    );
                }
            }
            Ok(())
        })();
        let result = measured
            .and_then(|()| Self::config_for(kind, backends, source_block_frames, spec, pools))
            .and_then(|config| {
                if keylock {
                    build_engine(config)
                } else {
                    build_varispeed_engine(config)
                }
            })
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
                pending.clear();
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
                let residency = SourceResidency::prepare(
                    pools,
                    reusable_residency,
                    history_frames,
                    resident_frames,
                    replacement_frames,
                    channels,
                )?;
                Ok((engine, pending, scratch, activation_scratch, residency))
            });
        result.map(
            |(engine, pending, scratch, activation_scratch, residency)| PreparedTarget {
                residency: Some(residency),
                activation_scratch: Some(activation_scratch),
                engine: Some(engine),
                pending_source: Some(pending),
                scratch: Some(scratch),
            },
        )
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
        drop(self.projection.retired.take());
        if self.prepared_quantum.is_none()
            && self
                .residency
                .as_ref()
                .is_none_or(|resident| resident.prepared.is_none())
        {
            self.projection.prepared = None;
        }
        self.projection.selected = self.plan_slot.load();
        if self.transition_pending() && spec == self.spec {
            self.service_scratch();
            return;
        }
        if (self.prepared_quantum.is_some()
            || self
                .residency
                .as_ref()
                .is_some_and(|resident| resident.prepared.is_some()))
            && spec == self.spec
        {
            self.service_scratch();
            return;
        }
        let channels = usize::from(self.spec.channels.max(1));
        if self.projection.selected.is_none() && self.projection.active.is_some() {
            if self.active || self.pending_frames(channels) > 0 {
                self.backend_transition_pending = true;
                self.service_scratch();
                return;
            }
            self.projection.retired = self.projection.active.take();
            self.projection.cursor = None;
            self.projection.output_frames = 0;
        }
        self.sync_plan();

        if spec.sample_rate != self.spec.sample_rate
            && let Some(applied) = self.applied_speed.as_mut()
        {
            applied.update_sample_rate(spec.sample_rate);
        }

        let kind = self.controls.backend();
        let keylock = self.controls.keylock();
        let entering_unity = spec == self.spec
            && (self.active || self.pending_frames(channels) > 0)
            && self.unity_passthrough(self.controls.speed());
        if entering_unity {
            self.service_scratch();
            return;
        }
        let backend_changed = kind != self.current_kind || keylock != self.current_keylock;
        if backend_changed
            && spec == self.spec
            && (self.active || self.pending_frames(channels) > 0)
        {
            self.backend_transition_pending = true;
            self.service_scratch();
            return;
        }
        if backend_changed || spec != self.spec || self.rebuild_pending {
            drop(self.deferred_scratch.take());
            if spec != self.spec || self.rebuild_pending {
                self.clear_render_state();
            }
            self.rebuild_pending = false;
            if spec != self.spec {
                for buffer in [&mut self.pending_source, &mut self.activation_scratch] {
                    if let Some(buffer) = buffer.as_mut() {
                        buffer.shrink_to_fit();
                    }
                }
                if spec.channels != self.spec.channels
                    && let Some(scratch) = self.scratch.as_mut()
                {
                    scratch.shrink_to_fit();
                }
                if let Some(resident) = self.residency.as_mut() {
                    resident.samples.shrink_to_fit();
                    resident.replacement.shrink_to_fit();
                }
            }
            let reusable = PreparedTarget {
                residency: self.residency.take(),
                activation_scratch: self.activation_scratch.take(),
                engine: self.engine.take(),
                pending_source: self.pending_source.take(),
                scratch: self.scratch.take(),
            };
            let target = Self::prepare_target(
                (kind, keylock),
                self.backends,
                self.source_block_frames,
                spec,
                &self.pools,
                reusable,
                spec != self.spec,
            )
            .unwrap_or_else(|error| {
                warn!(%kind, %error, "time-stretch engine preparation failed");
                PreparedTarget::default()
            });
            self.residency = target.residency;
            self.activation_scratch = target.activation_scratch;
            self.engine = target.engine;
            self.pending_source = target.pending_source;
            self.scratch = target.scratch;
            self.current_kind = kind;
            self.current_keylock = keylock;
            self.applied_pitch = f64::NAN;
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
