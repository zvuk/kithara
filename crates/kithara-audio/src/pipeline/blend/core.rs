use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_decode::BlenderProfile;
use kithara_signal::{AudioChunk, AudioSpec};

struct Consts;

impl Consts {
    const JOIN_MICROS: u32 = 20_000;
    const MICROS_PER_SEC: u32 = 1_000_000;
    const MIN_JOIN_FRAMES: u16 = 2;
}

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
    active: BlenderProfile,
    join: JoinState,
    outgoing: SampleBuffer,
    prepared: BlenderProfile,
    prepared_outgoing: SampleBuffer,
}

impl GaplessBlender {
    pub(crate) fn new<S>(active: BlenderProfile, pools: &PoolRegion<S>) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let samples = join_samples(active.spec());
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
        let channels = usize::from(chunk.spec().channels.max(1));
        for samples in chunk.samples.chunks_exact_mut(channels) {
            if *frame >= *frames {
                break;
            }
            let incoming_gain = f32::from(*frame) / f32::from(*frames);
            let outgoing_gain = 1.0 - incoming_gain;
            let outgoing_start = usize::from(*frame).saturating_mul(channels);
            for (channel, sample) in samples.iter_mut().enumerate() {
                let outgoing = self.outgoing[outgoing_start + channel];
                if outgoing.to_bits() != sample.to_bits() {
                    *sample = outgoing.mul_add(outgoing_gain, *sample * incoming_gain);
                }
            }
            *frame = frame.saturating_add(1);
        }
        if *frame >= *frames {
            self.join = JoinState::Steady;
        }
    }

    pub(crate) fn is_steady(&self) -> bool {
        matches!(self.join, JoinState::Steady)
    }

    pub(crate) fn prepare_active(&mut self, active: BlenderProfile) -> Result<(), PoolError> {
        self.prepared_outgoing.clear();
        self.prepared_outgoing
            .ensure_len(join_samples(active.spec()))?;
        self.prepared = active;
        Ok(())
    }

    pub(crate) fn prepare_join(&mut self, copy_outgoing: impl FnOnce(&mut [f32]) -> bool) -> bool {
        if !self.is_steady() || self.active.spec() != self.prepared.spec() {
            return false;
        }
        copy_outgoing(&mut self.prepared_outgoing)
    }

    pub(crate) fn commit_join(&mut self) {
        self.swap_prepared();
        self.join = JoinState::Active {
            frame: 0,
            frames: join_frames(self.active.spec()),
        };
    }

    pub(crate) fn join_frame_count(&self) -> u64 {
        join_frame_count(self.active.spec())
    }

    pub(crate) fn process_active(&mut self, mut chunk: AudioChunk) -> AudioChunk {
        debug_assert_eq!(chunk.spec(), self.active.spec());
        self.apply_join(&mut chunk);
        chunk
    }

    pub(crate) fn replace_active(&mut self, active: BlenderProfile) {
        if self.active.spec() == active.spec() {
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

    #[cfg(test)]
    pub(super) fn buffer_capacities(&self) -> (usize, usize) {
        (self.outgoing.capacity(), self.prepared_outgoing.capacity())
    }
}

fn join_frames(spec: AudioSpec) -> u16 {
    u16::try_from(
        u64::from(spec.sample_rate.get())
            .saturating_mul(u64::from(Consts::JOIN_MICROS))
            .div_ceil(u64::from(Consts::MICROS_PER_SEC)),
    )
    .unwrap_or(u16::MAX)
    .max(Consts::MIN_JOIN_FRAMES)
}

pub(crate) fn join_frame_count(spec: AudioSpec) -> u64 {
    u64::from(join_frames(spec))
}

fn join_samples(spec: AudioSpec) -> usize {
    usize::from(join_frames(spec)).saturating_mul(usize::from(spec.channels.max(1)))
}
