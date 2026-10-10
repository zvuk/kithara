use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_signal::{AudioSpec, SampleCount};
use kithara_stretch::{
    BackendCapabilities, ElasticBackendConfig, ElasticConfig, ElasticEngine, ElasticError,
    StretchKind, build_engine, build_varispeed_engine,
};
use num_traits::ToPrimitive;
use tracing::warn;

use super::{core::WarpRenderer, residency::SourceResidency, transition::RetiringTarget};

#[derive(Default)]
pub(in crate::render) struct PreparedTarget {
    pub(in crate::render) projection: Option<super::projected::Projection>,
    pub(in crate::render) activation_scratch: Option<SampleBuffer>,
    pub(in crate::render) engine: Option<Box<dyn ElasticEngine>>,
    pub(in crate::render) pending_source: Option<SampleBuffer>,
    pub(in crate::render) residency: Option<SourceResidency>,
    pub(in crate::render) scratch: Option<SampleBuffer>,
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(in crate::render) fn config_for(
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

    pub(in crate::render) fn prepare_target(
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
            projection: reusable_projection,
            residency: reusable_residency,
            activation_scratch: reusable_activation_scratch,
            engine: reusable_engine,
            pending_source: reusable_pending,
            scratch: reusable_scratch,
        } = reusable;
        drop(reusable_projection);
        drop(reusable_engine);
        let channels = usize::from(spec.channels.max(1));
        let mut history_frames = reusable_residency
            .as_ref()
            .map_or(0, |resident| resident.history_frames)
            .max(32);
        let mut resident_frames = reusable_residency.as_ref().map_or_else(
            || source_block_frames.get(),
            |resident| resident.samples.capacity() / channels,
        );
        let mut replacement_frames = reusable_residency
            .as_ref()
            .map_or(0, |resident| resident.replacement.capacity() / channels);
        let measured = (|| -> Result<(), ElasticError> {
            for backend in StretchKind::all().iter().filter(|backend| {
                measure_capabilities && backend.capabilities().contains(BackendCapabilities::RATE)
            }) {
                let capabilities =
                    Self::config_for(*backend, backends, source_block_frames, spec, pools)
                        .and_then(build_engine)
                        .map(|engine| engine.capabilities())?;
                {
                    let latency = capabilities.latency();
                    history_frames = history_frames.max(latency.first().saturating_mul(12));
                    let source_tail = Self::latency_source_tail(capabilities);
                    replacement_frames =
                        replacement_frames.max(latency.second().saturating_add(source_tail));
                    let warm = Self::latency_warm_source(capabilities);
                    resident_frames = resident_frames.max(
                        latency
                            .first()
                            .saturating_mul(2)
                            .saturating_add(warm)
                            .saturating_add(source_block_frames.get().saturating_mul(2)),
                    );
                    resident_frames = resident_frames.max(
                        history_frames
                            .saturating_add(
                                latency
                                    .first()
                                    .saturating_add(latency.second())
                                    .saturating_mul(12),
                            )
                            .saturating_add(source_block_frames.get().saturating_mul(4)),
                    );
                }
            }
            Ok(())
        })();
        let result = measured
            .and_then(|()| Self::config_for(kind, backends, source_block_frames, spec, pools))
            .and_then(|config| {
                if keylock || !kind.capabilities().contains(BackendCapabilities::RATE) {
                    build_engine(config)
                } else {
                    build_varispeed_engine(config)
                }
            })
            .and_then(|engine| {
                let channels = usize::from(spec.channels.max(1));
                let latency = engine.capabilities().latency();
                let projection_frames = latency
                    .first()
                    .checked_mul(6)
                    .and_then(|history| {
                        latency
                            .second()
                            .checked_mul(3)
                            .and_then(|future| history.checked_add(future))
                    })
                    .ok_or(ElasticError::SampleCountOverflow)?;
                let pending_samples = SampleCount::new(
                    source_block_frames
                        .get()
                        .max(projection_frames)
                        .checked_mul(channels)
                        .ok_or(ElasticError::SampleCountOverflow)?,
                );
                let pending = Self::prepare_buffer(pools, reusable_pending, pending_samples.get())?;
                let scratch_samples = Self::scratch_samples(engine.as_ref(), spec)?;
                let scratch = Self::prepare_buffer(pools, reusable_scratch, scratch_samples.get())?;
                let activation_samples = engine
                    .capabilities()
                    .latency()
                    .second()
                    .checked_mul(channels)
                    .ok_or(ElasticError::SampleCountOverflow)?;
                let activation_scratch =
                    Self::prepare_buffer(pools, reusable_activation_scratch, activation_samples)?;
                let residency = SourceResidency::prepare(
                    pools,
                    reusable_residency,
                    history_frames,
                    resident_frames,
                    replacement_frames,
                    channels,
                )?;
                let projection = keylock
                    .then(|| {
                        Self::prepare_projection(
                            kind,
                            backends,
                            source_block_frames,
                            spec,
                            pools,
                            projection_frames,
                        )
                    })
                    .transpose()?;
                Ok((
                    engine,
                    pending,
                    scratch,
                    activation_scratch,
                    residency,
                    projection,
                ))
            });
        result.map(
            |(engine, pending, scratch, activation_scratch, residency, projection)| {
                PreparedTarget {
                    projection,
                    residency: Some(residency),
                    activation_scratch: Some(activation_scratch),
                    engine: Some(engine),
                    pending_source: Some(pending),
                    scratch: Some(scratch),
                }
            },
        )
    }

    pub(in crate::render) fn prepare_buffer(
        pools: &PoolRegion<S>,
        reusable: Option<SampleBuffer>,
        samples: usize,
    ) -> Result<SampleBuffer, ElasticError> {
        let mut buffer = reusable.unwrap_or_else(|| pools.get::<f32>());
        buffer
            .ensure_len(samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        buffer.truncate(samples);
        buffer.shrink_to_fit();
        buffer.clear();
        Ok(buffer)
    }

    fn latency_source_tail(capabilities: kithara_stretch::ElasticCapabilities) -> usize {
        (capabilities.latency().first().to_f64().unwrap_or(f64::MAX)
            / capabilities.rate_envelope().min_source_frames_per_output())
        .ceil()
        .to_usize()
        .unwrap_or(usize::MAX)
    }

    fn latency_warm_source(capabilities: kithara_stretch::ElasticCapabilities) -> usize {
        (capabilities.latency().second().to_f64().unwrap_or(f64::MAX)
            * capabilities.rate_envelope().max_source_frames_per_output())
        .ceil()
        .to_usize()
        .unwrap_or(usize::MAX)
    }

    fn prepare_projection(
        kind: StretchKind,
        backends: ElasticBackendConfig,
        source_block_frames: NonZeroUsize,
        spec: AudioSpec,
        pools: &PoolRegion<S>,
        projection_frames: usize,
    ) -> Result<super::projected::Projection, ElasticError> {
        let first = Self::config_for(kind, backends, source_block_frames, spec, pools)
            .and_then(build_engine)?;
        let second = Self::config_for(kind, backends, source_block_frames, spec, pools)
            .and_then(build_engine)?;
        let samples = projection_frames
            .checked_mul(usize::from(spec.channels.max(1)))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let mut first_buffer = Self::prepare_buffer(pools, None, samples)?;
        let mut second_buffer = Self::prepare_buffer(pools, None, samples)?;
        first_buffer
            .ensure_len(samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        second_buffer
            .ensure_len(samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        Ok(super::projected::Projection {
            engines: [first, second],
            buffers: [first_buffer, second_buffer],
            stages: 1,
        })
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
        if !self.requires_staging() && self.plan.is_some() {
            drop(self.deferred_scratch.take());
            return;
        }
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
    pub(in crate::render) fn service_target(&mut self, spec: AudioSpec) {
        if self
            .retiring_target
            .as_ref()
            .is_some_and(RetiringTarget::complete)
        {
            self.retiring_target = None;
        }
        self.reprime_pending |= self.active && self.mapped_render && self.keylocked_unity();
        drop(self.retired_engine.take());
        if (self.transition_pending() || self.prepared_quantum.is_some()) && spec == self.spec {
            self.service_scratch();
            return;
        }
        let channels = usize::from(self.spec.channels.max(1));

        let (kind, keylock) = self.stretch_target();
        let entering_unity = spec == self.spec
            && (self.active || self.pending_frames(channels) > 0)
            && self.unity_passthrough(self.rate.speed());
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
            if spec != self.spec
                || (self.rebuild_pending && self.retiring_target.is_none() && self.engine.is_none())
            {
                self.clear_render_state();
            }
            self.rebuild_pending = false;
            let mut reusable = PreparedTarget {
                projection: self.projection.take(),
                residency: self.residency.take(),
                activation_scratch: self.activation_scratch.take(),
                engine: self.engine.take(),
                pending_source: self.pending_source.take(),
                scratch: self.scratch.take(),
            };
            if spec != self.spec {
                reusable = PreparedTarget::default();
            }
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
            self.projection = target.projection;
            self.activation_scratch = target.activation_scratch;
            self.engine = target.engine;
            self.pending_source = target.pending_source;
            self.scratch = target.scratch;
            self.current_kind = kind;
            self.current_keylock = keylock;
            self.applied_pitch = f64::NAN;
            self.spec = spec;
            self.reset_pending = false;
            let latency = self.engine_latency().get();
            if let Some(target) = self.retiring_target.as_mut() {
                target.extend(latency);
            }
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
