use std::num::NonZeroUsize;

use kithara_blob::BlobError;
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_dsp::downmix;

use crate::{
    KeyError, KeyEstimate, KeyResume, correlation, grid::FrameGrid, profile::FrameProfile,
};

/// Estimates a track's global key from ranges of its PCM that may arrive in
/// any order, with gaps, or twice.
pub struct KeyEstimator {
    grid: FrameGrid,
    profile: FrameProfile,
    mono: SampleBuffer,
}

impl KeyEstimator {
    /// # Errors
    /// [`KeyError`] when the FFT cannot be built or its buffers do not fit
    /// `pools`.
    pub fn new<S>(sample_rate: u32, pools: &PoolRegion<S>) -> Result<Self, KeyError>
    where
        S: HasPool<f32>,
    {
        Ok(Self {
            profile: FrameProfile::new(sample_rate, pools)?,
            grid: FrameGrid::new(pools)?,
            mono: pools.get::<f32>(),
        })
    }

    /// The key of the frames taken so far, or `None` while none carries tonal
    /// energy.
    #[must_use]
    pub fn estimate(&self) -> Option<KeyEstimate> {
        correlation::estimate(&self.grid.totals.sum)
    }

    /// Takes interleaved `pcm` of `channels` channels starting at source
    /// frame `at`, folded to mono; mono `pcm` is taken as it is.
    ///
    /// # Errors
    /// [`KeyError`] when a frame buffer does not fit `pools`.
    pub fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: NonZeroUsize,
        at: u64,
    ) -> Result<(), KeyError>
    where
        S: HasPool<f32>,
    {
        if channels == NonZeroUsize::MIN {
            return self.grid.push(&mut self.profile, pools, pcm, at);
        }
        let frames = pcm.len() / channels;
        self.mono.ensure_len(frames)?;
        let written = downmix(pcm, channels, &mut self.mono);
        let mono = self.mono.get(..written).unwrap_or_default();
        self.grid.push(&mut self.profile, pools, mono, at)
    }

    /// Continues from `resume` in place of the frames taken so far.
    ///
    /// # Errors
    /// [`BlobError::Pool`] when the pending frames do not fit `pools`.
    pub fn restore_state<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: KeyResume,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        self.grid.restore(pools, resume)
    }

    /// States that the source ends at frame `frames`, so the frames reaching
    /// past it count the missing samples as zeros.
    ///
    /// # Errors
    /// [`KeyError`] when a closed frame cannot be transformed.
    pub fn set_end(&mut self, frames: u64) -> Result<(), KeyError> {
        self.grid.set_end(&mut self.profile, frames)
    }

    /// Appends the whole state as versioned bytes that [`KeyResume::decode`]
    /// reads.
    pub fn write_state(&self, out: &mut Vec<u8>) {
        self.grid.write_state(out);
    }
}
