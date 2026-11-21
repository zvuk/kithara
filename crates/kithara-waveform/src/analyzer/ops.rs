use std::collections::btree_map::Entry;

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_dsp::filter::FilterError;
use kithara_signal::FrameCoverage;
use rangemap::RangeSet;
use tracing::debug;

use super::{
    WaveformAnalyzer,
    waveform::{Columns, Partial, body},
};
use crate::{AnalyzerError, consts};

impl WaveformAnalyzer {
    pub(super) fn evict_overflow(&mut self) {
        while self.partial.len() > consts::MAX_PARTIAL {
            let oldest = self
                .partial
                .iter()
                .min_by_key(|(_, held)| held.seq)
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

    delegate::delegate! {
        to self {
            #[expr(u64::try_from($ / 4).unwrap_or(1))]
            #[call(window_size)]
            pub(super) fn hop(&self) -> u64;
            #[expr(u64::try_from($).unwrap_or(0))]
            #[call(window_size)]
            pub(super) fn size(&self) -> u64;
        }
    }

    pub(super) fn window_size(&self) -> usize {
        self.column_frames * consts::WINDOW_COLUMNS
    }

    pub(super) fn reduce_if_complete(&mut self, index: u64) -> Result<(), AnalyzerError> {
        let start = index.saturating_mul(self.hop());
        let end = start.saturating_add(self.size());
        if !self
            .partial
            .get(&index)
            .is_some_and(|held| held.written.covers(&(start..end)))
        {
            return Ok(());
        }
        let Some(held) = self.partial.remove(&index) else {
            return Ok(());
        };
        let columns = self
            .envelope
            .reduce(&held.samples, index == 0, self.window_size())?;
        self.columns.insert(index, columns);
        Ok(())
    }

    pub(super) fn tail_columns(
        &mut self,
        extent: Option<u64>,
    ) -> Result<Option<(u64, Columns)>, AnalyzerError> {
        let Some(extent) = extent.filter(|extent| *extent > 0) else {
            return Ok(None);
        };
        let body = (extent - 1) / (self.hop() * 3);
        let index = if body == 0 { 0 } else { body * 3 - 1 };
        let Some(held) = self.partial.get(&index) else {
            return Ok(None);
        };
        let start = index.saturating_mul(self.hop());
        if !held.written.covers(&(start..extent)) {
            return Ok(None);
        }
        let frames = usize::try_from(extent - start).map_err(|_| FilterError::Shape)?;
        Ok(Some((
            index,
            self.envelope.reduce(&held.samples, index == 0, frames)?,
        )))
    }

    pub(super) fn scatter<S>(
        &mut self,
        pools: &PoolRegion<S>,
        index: u64,
        pcm: &[f32],
        channels: usize,
        at: u64,
        end: u64,
    ) -> Result<(), PoolError>
    where
        S: HasPool<f32>,
    {
        if self.columns.contains_key(&index) {
            return Ok(());
        }
        let start = index.saturating_mul(self.hop());
        let from = start.max(at);
        let to = start.saturating_add(self.size()).min(end);
        let (Ok(offset), Ok(source), Ok(len)) = (
            usize::try_from(from - start),
            usize::try_from(from - at),
            usize::try_from(to - from),
        ) else {
            return Ok(());
        };
        let window_size = self.window_size();
        let partial = match self.partial.entry(index) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let seq = self.opened;
                self.opened = seq.saturating_add(1);
                entry.insert(Partial {
                    samples: pools.get_with_len::<f32>(window_size * 2)?,
                    written: RangeSet::new(),
                    seq,
                })
            }
        };
        let target = &mut partial.samples[offset * 2..(offset + len) * 2];
        let source = &pcm[source * channels..(source + len) * channels];
        for (target, frame) in target
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(source.chunks_exact(channels))
        {
            target.copy_from_slice(&[frame[0], frame[usize::from(channels > 1)]]);
        }
        partial.written.insert(from..to);
        Ok(())
    }

    pub(super) fn column_count(&self, extent: Option<u64>) -> Result<u64, AnalyzerError> {
        if let Some(extent) = extent {
            let frames = u64::try_from(self.column_frames).map_err(|_| FilterError::Shape)?;
            return Ok(extent.div_ceil(frames));
        }
        let Some(last) = self.columns.keys().next_back() else {
            return Ok(0);
        };
        body(*last)
            .checked_add(1)
            .and_then(|count| count.checked_mul(consts::COLUMNS as u64))
            .ok_or_else(|| FilterError::Shape.into())
    }
}
