use std::{array, collections::btree_map::Entry};

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_dsp::sum_squares;
use kithara_signal::FrameCoverage;
use num_traits::cast::ToPrimitive;
use rangemap::RangeSet;
use tracing::debug;

use super::{
    WaveformAnalyzer,
    waveform::{Partial, consts},
};
use crate::Band;

impl WaveformAnalyzer {
    pub(super) fn evict_overflow(&mut self) {
        while self.partial.len() > consts::MAX_PARTIAL {
            let oldest = self
                .partial
                .iter()
                .min_by_key(|(_, partial)| partial.seq)
                .map(|(index, _)| *index);
            let Some(index) = oldest else {
                return;
            };
            self.partial.remove(&index);
            debug!(
                index,
                "waveform: partial window evicted; span left unanalysed"
            );
        }
    }

    pub(super) fn hop(&self) -> u64 {
        self.window_hop.get()
    }

    #[cfg(test)]
    pub(super) fn partial_len(&self) -> usize {
        self.partial.len()
    }

    fn reduce(&mut self, index: u64, samples: &[f32]) {
        let bands = if self.fft.forward(samples, &mut self.spectrum).is_ok() {
            self.window_bands()
        } else {
            [0.0; Band::COUNT]
        };
        self.bands.insert(index, bands);
    }

    pub(super) fn reduce_if_complete(&mut self, index: u64) {
        if self.bands.contains_key(&index) {
            return;
        }
        let start = index.saturating_mul(self.hop());
        let span = start..start.saturating_add(self.size());
        if !self
            .partial
            .get(&index)
            .is_some_and(|partial| partial.written.covers(&span))
        {
            return;
        }
        let Some(partial) = self.partial.remove(&index) else {
            return;
        };
        self.reduce(index, &partial.samples);
    }

    pub(super) fn reduce_padded(&mut self, extent: u64) {
        if extent == 0 || extent >= self.size() {
            return;
        }
        let Some(partial) = self.partial.remove(&0) else {
            return;
        };
        let covered = usize::try_from(extent)
            .unwrap_or(usize::MAX)
            .min(partial.samples.len());
        self.reduce(0, &partial.samples[..covered]);
    }

    #[cfg(test)]
    pub(super) fn reduced(&self, index: u64) -> Option<[f32; Band::COUNT]> {
        self.bands.get(&index).copied()
    }

    /// The caller hands only windows whose span overlaps the clipped range, so that range is never
    /// empty and needs no second overlap test.
    pub(super) fn scatter<S>(
        &mut self,
        pools: &PoolRegion<S>,
        index: u64,
        mono: &[f32],
        at: u64,
        end: u64,
    ) -> Result<(), PoolError>
    where
        S: HasPool<f32>,
    {
        if self.bands.contains_key(&index) {
            return Ok(());
        }
        let size = self.size();
        let start = index.saturating_mul(self.hop());
        let from = start.max(at);
        let to = start.saturating_add(size).min(end);
        let (Ok(offset), Ok(source), Ok(len)) = (
            usize::try_from(from - start),
            usize::try_from(from - at),
            usize::try_from(to.saturating_sub(from)),
        ) else {
            return Ok(());
        };
        let window_size = self.window_size();

        let partial = match self.partial.entry(index) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let opened = self.opened;
                self.opened = opened.saturating_add(1);
                entry.insert(Partial {
                    samples: pools.get_with_len::<f32>(window_size)?,
                    written: RangeSet::new(),
                    seq: opened,
                })
            }
        };
        let (Some(dst), Some(src)) = (
            partial.samples.get_mut(offset..offset + len),
            mono.get(source..source + len),
        ) else {
            return Ok(());
        };
        dst.copy_from_slice(src);
        partial.written.insert(from..to);
        Ok(())
    }

    pub(super) fn size(&self) -> u64 {
        u64::try_from(self.window_size()).unwrap_or(0)
    }

    /// Leaves the DC bin out so a constant offset never colors the low band;
    /// a crossover at either end of the spectrum empties its band.
    fn window_bands(&self) -> [f32; Band::COUNT] {
        let (re, im) = (self.spectrum.re(), self.spectrum.im());
        let [low, mid, high] = [1, self.low_mid_bin.max(1), self.mid_high_bin.max(1)];
        let band = [low..mid, mid..high, high..re.len()]
            .map(|bins| sum_squares(&re[bins.clone()]) + sum_squares(&im[bins]));
        let total: f32 = band.iter().sum();
        let rms = (total / self.window_size().to_f32().unwrap_or(1.0)).sqrt();
        if rms < self.params.energy_floor() {
            return [0.0; Band::COUNT];
        }
        array::from_fn(|i| band[i] * self.band_bin_inv[i])
    }

    pub(super) fn window_count(&self, extent: Option<u64>) -> usize {
        let slots = match extent {
            Some(extent) if extent >= self.size() => (extent - self.size()) / self.hop() + 1,
            Some(_) => u64::from(!self.bands.is_empty()),
            None => self.bands.keys().next_back().map_or(0, |last| last + 1),
        };
        usize::try_from(slots).unwrap_or(usize::MAX)
    }

    pub(super) fn window_size(&self) -> usize {
        self.fft.size().get()
    }
}
