use kithara_bufpool::{ByteBuffer, HasPool, PoolError, PoolRegion};
use kithara_signal::AudioChunk;

use crate::{
    AudioEffect, EffectPrepareError,
    chain::{check_geometry, check_input},
};

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum StageState {
    UpstreamActive,
    Flushing,
    Exhausted,
}

pub enum EffectDrainStep {
    Produced(Box<AudioChunk>),
    Progress,
    Exhausted,
}

pub struct EffectDrain {
    exhausted: ByteBuffer,
    active: bool,
}

impl EffectDrain {
    /// Allocate a drain able to track `effect_count` stages.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError`] when the region cannot hand out the
    /// one-byte-per-stage buffer this drain keeps its exhaustion flags in.
    pub fn new<S>(effect_count: usize, pools: &PoolRegion<S>) -> Result<Self, PoolError>
    where
        S: HasPool<u8>,
    {
        Ok(Self {
            exhausted: pools.get_with_len::<u8>(effect_count)?,
            active: false,
        })
    }

    pub const fn reset(&mut self) {
        self.active = false;
    }

    /// Advance the chain's finite tail without silently discarding invalid PCM.
    /// # Errors
    /// Refuses an unsupported format or incomplete interleaved frame.
    pub fn step(
        &mut self,
        effects: &mut [Box<dyn AudioEffect>],
    ) -> Result<EffectDrainStep, EffectPrepareError> {
        if effects.is_empty() {
            return Ok(EffectDrainStep::Exhausted);
        }
        if !self.active {
            self.exhausted.fill(StageState::UpstreamActive as u8);
            self.active = true;
        }
        pull(effects, &mut self.exhausted, effects.len() - 1)
    }
}

fn pull(
    effects: &mut [Box<dyn AudioEffect>],
    exhausted: &mut [u8],
    stage: usize,
) -> Result<EffectDrainStep, EffectPrepareError> {
    if stage == 0 {
        return flush_stage(effects, exhausted, stage);
    }

    if exhausted[stage] == StageState::UpstreamActive as u8 {
        match pull(effects, exhausted, stage - 1)? {
            EffectDrainStep::Produced(chunk) => {
                check_input(&*effects[stage], &chunk)?;
                let Some(output) = effects[stage].process(*chunk) else {
                    return Ok(EffectDrainStep::Progress);
                };
                check_geometry(&output)?;
                return Ok(EffectDrainStep::Produced(Box::new(output)));
            }
            EffectDrainStep::Progress => return Ok(EffectDrainStep::Progress),
            EffectDrainStep::Exhausted => exhausted[stage] = StageState::Flushing as u8,
        }
    }
    flush_stage(effects, exhausted, stage)
}

fn flush_stage(
    effects: &mut [Box<dyn AudioEffect>],
    exhausted: &mut [u8],
    stage: usize,
) -> Result<EffectDrainStep, EffectPrepareError> {
    if exhausted[stage] == StageState::Exhausted as u8 {
        return Ok(EffectDrainStep::Exhausted);
    }
    let Some(chunk) = effects[stage].flush() else {
        exhausted[stage] = StageState::Exhausted as u8;
        return Ok(EffectDrainStep::Exhausted);
    };
    check_input(&*effects[stage], &chunk)?;
    Ok(EffectDrainStep::Produced(Box::new(chunk)))
}
