use std::{
    collections::{BTreeMap, btree_map::Entry},
    ops::Range,
};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use rangemap::RangeSet;

use crate::{KeyError, consts, profile::FrameProfile};

pub(crate) struct FrameGrid {
    pub(crate) pending: BTreeMap<u64, Pending>,
    pub(crate) end: Option<u64>,
    pub(crate) totals: Totals,
    scratch: SampleBuffer,
}

pub(crate) struct Totals {
    pub(crate) evaluated: RangeSet<u64>,
    pub(crate) sum: [f64; consts::PITCH_CLASSES],
}

pub(crate) struct Pending {
    pub(crate) written: RangeSet<u64>,
    pub(crate) samples: SampleBuffer,
}

impl FrameGrid {
    pub(crate) fn new<S>(pools: &PoolRegion<S>) -> Result<Self, KeyError>
    where
        S: HasPool<f32>,
    {
        Ok(Self {
            totals: Totals {
                sum: [0.0; consts::PITCH_CLASSES],
                evaluated: RangeSet::new(),
            },
            end: None,
            pending: BTreeMap::new(),
            scratch: pools.get_with_len::<f32>(frame_len())?,
        })
    }

    pub(crate) fn push<S>(
        &mut self,
        profile: &mut FrameProfile,
        pools: &PoolRegion<S>,
        mono: &[f32],
        at: u64,
    ) -> Result<(), KeyError>
    where
        S: HasPool<f32>,
    {
        let mut len = u64::try_from(mono.len()).unwrap_or(u64::MAX);
        if let Some(end) = self.end {
            len = len.min(end.saturating_sub(at));
        }
        if len == 0 {
            return Ok(());
        }
        let from = at.saturating_add(consts::FRAME_LEAD);
        let to = from.saturating_add(len);
        for index in from / frame_size()..=(to - 1) / frame_size() {
            if self.totals.evaluated.contains(&index) {
                continue;
            }
            let start = index * frame_size();
            let span = from.max(start) - start..to.min(start.saturating_add(frame_size())) - start;
            let chunk = offset(start + span.start - from)..offset(start + span.end - from);
            let Some(chunk) = mono.get(chunk) else {
                continue;
            };
            let taken = if self.is_whole(index, &span) {
                self.add_whole(profile, index, &span, chunk)
            } else {
                self.accumulate(profile, pools, index, span, chunk)
            };
            taken?;
        }
        Ok(())
    }

    pub(crate) fn set_end(&mut self, profile: &mut FrameProfile, end: u64) -> Result<(), KeyError> {
        self.end = Some(end);
        self.pending
            .retain(|&index, _| !needed(Some(end), index).is_empty());
        let ready = self.pending.extract_if(.., |&index, frame| {
            frame
                .written
                .gaps(&needed(Some(end), index))
                .next()
                .is_none()
        });
        for (index, frame) in ready {
            self.totals.add(profile, index, &frame.samples)?;
        }
        Ok(())
    }

    /// Whether `span` covers all of frame `index` that the source holds and
    /// nothing of the frame arrived before.
    fn is_whole(&self, index: u64, span: &Range<u64>) -> bool {
        let needed = needed(self.end, index);
        !self.pending.contains_key(&index) && span.start <= needed.start && span.end >= needed.end
    }

    fn add_whole(
        &mut self,
        profile: &mut FrameProfile,
        index: u64,
        span: &Range<u64>,
        chunk: &[f32],
    ) -> Result<(), KeyError> {
        self.scratch.fill(0.0);
        copy_into(&mut self.scratch, span, chunk);
        self.totals.add(profile, index, &self.scratch)
    }

    /// Keeps part of frame `index` until the rest of it arrives.
    fn accumulate<S>(
        &mut self,
        profile: &mut FrameProfile,
        pools: &PoolRegion<S>,
        index: u64,
        span: Range<u64>,
        chunk: &[f32],
    ) -> Result<(), KeyError>
    where
        S: HasPool<f32>,
    {
        let pending = match self.pending.entry(index) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(Pending {
                samples: pools.get_with_len::<f32>(frame_len())?,
                written: RangeSet::new(),
            }),
        };
        copy_into(&mut pending.samples, &span, chunk);
        pending.written.insert(span);
        if pending
            .written
            .gaps(&needed(self.end, index))
            .next()
            .is_some()
        {
            return Ok(());
        }
        if let Some(frame) = self.pending.remove(&index) {
            self.totals.add(profile, index, &frame.samples)?;
        }
        Ok(())
    }
}

impl Totals {
    fn add(
        &mut self,
        profile: &mut FrameProfile,
        index: u64,
        frame: &[f32],
    ) -> Result<(), KeyError> {
        let bins = profile.profile(frame)?;
        for (total, bin) in self.sum.iter_mut().zip(bins) {
            *total += f64::from(bin);
        }
        self.evaluated.insert(index..index + 1);
        Ok(())
    }
}

fn needed(end: Option<u64>, index: u64) -> Range<u64> {
    let start = index.saturating_mul(frame_size());
    let first = consts::FRAME_LEAD.saturating_sub(start);
    let last = end.map_or_else(frame_size, |end| {
        end.saturating_add(consts::FRAME_LEAD)
            .saturating_sub(start)
            .min(frame_size())
    });
    first..last.max(first)
}

fn copy_into(frame: &mut [f32], span: &Range<u64>, chunk: &[f32]) {
    if let Some(slot) = frame.get_mut(offset(span.start)..offset(span.end)) {
        slot.copy_from_slice(chunk);
    }
}

pub(crate) fn frame_len() -> usize {
    consts::FRAME.get()
}

pub(crate) fn frame_size() -> u64 {
    u64::try_from(frame_len()).unwrap_or(u64::MAX)
}

fn offset(position: u64) -> usize {
    usize::try_from(position).unwrap_or(usize::MAX)
}
