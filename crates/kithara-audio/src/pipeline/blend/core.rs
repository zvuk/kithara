use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_dsp::crossfade_linear;
use kithara_signal::{AudioChunk, AudioSpec};

use crate::consts;

enum JoinState {
    Steady,
    /// Ramp counters are `u16` so the per-frame gain is an exact
    /// [`f32::from`] conversion: at 20 ms the ramp is under 4 000 frames even
    /// at 192 kHz, and the saturating conversion pins the pathological case.
    Active {
        frame: u16,
        frames: u16,
    },
}

pub(crate) struct GaplessBlender {
    active: AudioSpec,
    prepared: AudioSpec,
    join: JoinState,
    outgoing: SampleBuffer,
    prepared_outgoing: SampleBuffer,
}

impl GaplessBlender {
    pub(crate) fn new<S>(active: AudioSpec, pools: &PoolRegion<S>) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let samples = join_samples(active);
        let outgoing = pools.get_with_len::<f32>(samples)?;
        let prepared_outgoing = pools.get_with_len::<f32>(samples)?;
        Ok(Self {
            active,
            join: JoinState::Steady,
            outgoing,
            prepared: active,
            prepared_outgoing,
        })
    }

    fn apply_join(&mut self, chunk: &mut AudioChunk) {
        let JoinState::Active { frame, frames } = &mut self.join else {
            return;
        };
        let channels =
            NonZeroUsize::new(usize::from(chunk.spec().channels)).unwrap_or(NonZeroUsize::MIN);
        let outgoing_start = usize::from(*frame).saturating_mul(channels.get());
        let outgoing_end = usize::from(*frames).saturating_mul(channels.get());
        let processed = crossfade_linear(
            &self.outgoing[outgoing_start..outgoing_end],
            &mut chunk.samples,
            channels,
            usize::from(*frame),
            usize::from(*frames),
        );
        *frame = frame.saturating_add(u16::try_from(processed).unwrap_or(u16::MAX));
        if *frame >= *frames {
            self.join = JoinState::Steady;
        }
    }

    #[cfg(test)]
    pub(super) fn buffer_capacities(&self) -> (usize, usize) {
        (self.outgoing.capacity(), self.prepared_outgoing.capacity())
    }

    pub(crate) fn commit_join(&mut self) {
        self.swap_prepared();
        self.join = JoinState::Active {
            frame: 0,
            frames: join_frames(self.active),
        };
    }

    pub(crate) fn is_steady(&self) -> bool {
        matches!(self.join, JoinState::Steady)
    }

    pub(crate) fn join_frame_count(&self) -> u64 {
        u64::from(join_frames(self.active))
    }

    pub(crate) fn prepare_active(&mut self, active: AudioSpec) -> Result<(), PoolError> {
        self.prepared_outgoing.clear();
        self.prepared_outgoing.ensure_len(join_samples(active))?;
        self.prepared = active;
        Ok(())
    }

    pub(crate) fn prepare_join(&mut self, copy_outgoing: impl FnOnce(&mut [f32]) -> bool) -> bool {
        if !self.is_steady() || self.active != self.prepared {
            return false;
        }
        copy_outgoing(&mut self.prepared_outgoing)
    }

    pub(crate) fn process_active(&mut self, mut chunk: AudioChunk) -> AudioChunk {
        debug_assert_eq!(chunk.spec(), self.active);
        self.apply_join(&mut chunk);
        chunk
    }

    pub(crate) fn replace_active(&mut self, active: AudioSpec) {
        if self.active == active {
            self.active = active;
            self.join = JoinState::Steady;
            return;
        }
        self.prepared = active;
        self.swap_prepared();
        self.join = JoinState::Steady;
    }

    pub(crate) fn reset(&mut self) {
        self.join = JoinState::Steady;
    }

    fn swap_prepared(&mut self) {
        std::mem::swap(&mut self.active, &mut self.prepared);
        std::mem::swap(&mut self.outgoing, &mut self.prepared_outgoing);
    }
}

fn join_frames(spec: AudioSpec) -> u16 {
    u16::try_from(
        u64::from(spec.sample_rate.get())
            .saturating_mul(u64::from(consts::JOIN_MICROS))
            .div_ceil(u64::from(consts::MICROS_PER_SEC)),
    )
    .unwrap_or(u16::MAX)
    .max(consts::MIN_JOIN_FRAMES)
}

fn join_samples(spec: AudioSpec) -> usize {
    usize::from(join_frames(spec)).saturating_mul(usize::from(spec.channels.max(1)))
}
