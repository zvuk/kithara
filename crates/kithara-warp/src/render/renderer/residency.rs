use std::{collections::VecDeque, num::NonZeroU128};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_signal::{AudioChunkInfo, SourceSpan};
use kithara_stretch::ElasticError;
use num_traits::ToPrimitive;

use super::core::WarpRenderer;

/// A bounded decoded-source window, shared by activation and backend replacement.
/// It contains history and lookahead, never independently scheduled output.
pub(in crate::render) struct SourceResidency {
    pub(in crate::render) end: Option<u64>,
    pub(in crate::render) origin: Option<u64>,
    pub(in crate::render) replacement: SampleBuffer,
    /// The tail a retiring engine drains into, with what still fades out of
    /// `replacement` blended in; it becomes `replacement` once complete.
    pub(in crate::render) next_replacement: SampleBuffer,
    pub(in crate::render) samples: SampleBuffer,
    pub(in crate::render) start: i64,
    pub(in crate::render) history_frames: usize,
    mappings: VecDeque<SourceSpan>,
    pub(in crate::render) offset: usize,
    pub(in crate::render) replacement_offset: usize,
}

impl SourceResidency {
    /// Only the portion before the physical recording is known silence.
    pub(in crate::render) fn append(
        &mut self,
        meta: AudioChunkInfo,
        input: &[f32],
    ) -> Result<(), ElasticError> {
        let channels = usize::from(meta.spec.channels.max(1));
        let frames = input.len() / channels;
        if frames == 0 {
            return Ok(());
        }
        if self.end.is_none() {
            self.origin = Some(meta.frame_offset);
            let prefix = self
                .history_frames
                .min(usize::try_from(meta.frame_offset).unwrap_or(usize::MAX));
            self.start =
                i64::try_from(meta.frame_offset).map_err(|_| ElasticError::SampleCountOverflow)?;
            if meta.frame_offset == 0 {
                let history = self
                    .history_frames
                    .min(self.samples.capacity() / channels - frames);
                self.start =
                    -i64::try_from(history).map_err(|_| ElasticError::SampleCountOverflow)?;
                self.samples
                    .ensure_len(history * channels)
                    .map_err(|_| ElasticError::PoolCapacity)?;
                self.samples.fill(0.0);
            } else if prefix == 0 {
                self.samples.clear();
            }
            self.end = Some(meta.frame_offset);
        }
        let expected = self.end.ok_or(ElasticError::EmptySource)?;
        if expected != meta.frame_offset {
            return Err(ElasticError::DiscontinuousSource {
                expected: expected.to_f64().ok_or(ElasticError::SampleCountOverflow)?,
                actual: meta
                    .frame_offset
                    .to_f64()
                    .ok_or(ElasticError::SampleCountOverflow)?,
            });
        }
        self.make_room(input.len())?;
        self.samples
            .try_extend_from_slice(input)
            .map_err(|_| ElasticError::PoolCapacity)?;
        self.end = Some(
            expected
                .checked_add(u64::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?)
                .ok_or(ElasticError::SampleCountOverflow)?,
        );
        Ok(())
    }

    /// Fade `output` in from the retired engine's tail, continuing where the
    /// previous output left the fade.
    pub(in crate::render) fn blend_replacement(
        &mut self,
        output: &mut [f32],
        channels: usize,
    ) -> Result<(), ElasticError> {
        let available = self
            .replacement
            .len()
            .saturating_sub(self.replacement_offset);
        let blend_samples = output.len().min(available);
        let total_frames = self.replacement.len() / channels;
        for (offset, sample) in output[..blend_samples].iter_mut().enumerate() {
            let index = self.replacement_offset + offset;
            let mix = (index / channels + 1)
                .to_f32()
                .ok_or(ElasticError::SampleCountOverflow)?
                / total_frames
                    .max(1)
                    .to_f32()
                    .ok_or(ElasticError::SampleCountOverflow)?;
            *sample = self.replacement[index].mul_add(1.0 - mix, *sample * mix);
        }
        self.replacement_offset += blend_samples;
        if self.replacement_offset == self.replacement.len() {
            self.replacement.clear();
            self.replacement_offset = 0;
        }
        Ok(())
    }

    pub(in crate::render) fn clear(&mut self) {
        self.samples.clear();
        self.replacement.clear();
        self.next_replacement.clear();
        self.replacement_offset = 0;
        self.start = 0;
        self.offset = 0;
        self.end = None;
        self.origin = None;
        self.mappings.clear();
    }

    pub(in crate::render) fn remember_mapping(
        &mut self,
        span: SourceSpan,
    ) -> Result<(), ElasticError> {
        if let Some(previous) = self.mappings.back_mut()
            && let Some(joined) = previous.followed_by(span)
        {
            *previous = joined;
        } else {
            if self.mappings.len() == self.mappings.capacity() {
                return Err(ElasticError::PoolCapacity);
            }
            self.mappings.push_back(span);
        }
        let mut remaining =
            u64::try_from(self.history_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        let mut retained = 0;
        for mapping in self.mappings.iter_mut().rev() {
            if remaining == 0 {
                break;
            }
            let frames = mapping.output_frames().min(remaining);
            *mapping = mapping
                .for_output_range(mapping.output_frames() - frames..mapping.output_frames())
                .ok_or(ElasticError::SampleCountOverflow)?;
            remaining -= frames;
            retained += 1;
        }
        while self.mappings.len() > retained {
            self.mappings.pop_front();
        }
        Ok(())
    }

    pub(in crate::render) fn history_position(
        &self,
        mut before: u64,
    ) -> Option<(u128, NonZeroU128)> {
        for span in self.mappings.iter().rev() {
            if before <= span.output_frames() {
                return span.source_ratio_at(span.output_frames() - before);
            }
            before -= span.output_frames();
        }
        None
    }

    fn make_room(&mut self, samples: usize) -> Result<(), ElasticError> {
        if self.samples.len() + samples > self.samples.capacity() && self.offset > 0 {
            self.samples.copy_within(self.offset.., 0);
            self.samples.truncate(self.samples.len() - self.offset);
            self.offset = 0;
        }
        if self.samples.len() + samples > self.samples.capacity() {
            return Err(ElasticError::PoolCapacity);
        }
        Ok(())
    }

    pub(in crate::render) fn prepare<S: HasPool<f32>>(
        pools: &PoolRegion<S>,
        reusable: Option<Self>,
        history_frames: usize,
        resident_frames: usize,
        replacement_frames: usize,
        channels: usize,
    ) -> Result<Self, ElasticError> {
        let mut residency = if let Some(reusable) = reusable {
            reusable
        } else {
            let samples = |frames: usize| {
                frames
                    .checked_mul(channels)
                    .ok_or(ElasticError::SampleCountOverflow)
                    .and_then(|samples| WarpRenderer::<S>::prepare_buffer(pools, None, samples))
            };
            Self {
                history_frames,
                mappings: VecDeque::new(),
                samples: samples(resident_frames)?,
                replacement: samples(replacement_frames)?,
                next_replacement: samples(replacement_frames)?,
                replacement_offset: 0,
                start: 0,
                offset: 0,
                end: None,
                origin: None,
            }
        };
        let mapping_capacity = history_frames
            .checked_add(1)
            .ok_or(ElasticError::SampleCountOverflow)?;
        if residency.mappings.capacity() < mapping_capacity {
            residency
                .mappings
                .try_reserve(mapping_capacity - residency.mappings.len())
                .map_err(|_| ElasticError::PoolCapacity)?;
        }
        for (buffer, frames) in [
            (&mut residency.samples, resident_frames),
            (&mut residency.replacement, replacement_frames),
            (&mut residency.next_replacement, replacement_frames),
        ] {
            let length = buffer.len();
            buffer
                .ensure_len(
                    frames
                        .checked_mul(channels)
                        .ok_or(ElasticError::SampleCountOverflow)?,
                )
                .map_err(|_| ElasticError::PoolCapacity)?;
            buffer.shrink_to_fit();
            buffer.truncate(length);
        }
        residency.history_frames = history_frames;
        Ok(residency)
    }

    pub(in crate::render) fn range(
        &self,
        start: i64,
        end: u64,
        channels: usize,
    ) -> Result<std::ops::Range<usize>, ElasticError> {
        let first = start
            .checked_sub(self.start)
            .and_then(|frames| usize::try_from(frames).ok())
            .and_then(|frames| frames.checked_mul(channels))
            .ok_or(ElasticError::EnginePreparation(
                "required source history is no longer resident",
            ))?;
        let first = first
            .checked_add(self.offset)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let last = i64::try_from(end)
            .ok()
            .and_then(|end| end.checked_sub(self.start))
            .and_then(|frames| usize::try_from(frames).ok())
            .and_then(|frames| frames.checked_mul(channels))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let last = last
            .checked_add(self.offset)
            .ok_or(ElasticError::SampleCountOverflow)?;
        if last > self.samples.len() || first > last {
            return Err(ElasticError::EnginePreparation(
                "required source is not resident",
            ));
        }
        Ok(first..last)
    }

    pub(in crate::render) fn retain_from(&mut self, source: u64, channels: usize) {
        let first = i64::try_from(source)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(self.history_frames).unwrap_or(i64::MAX));
        let remove = usize::try_from(first.saturating_sub(self.start).max(0))
            .unwrap_or(usize::MAX)
            .saturating_mul(channels)
            .min(self.samples.len().saturating_sub(self.offset));
        self.offset += remove;
        self.start = self
            .start
            .saturating_add(i64::try_from(remove / channels).unwrap_or(i64::MAX));
    }

    pub(in crate::render) fn retain_manual(
        &mut self,
        mut meta: AudioChunkInfo,
        input: &[f32],
        frontier: Option<u64>,
    ) -> Result<(), ElasticError> {
        let channels = usize::from(meta.spec.channels.max(1));
        if let Some(frontier) = frontier {
            self.retain_from(frontier, channels);
        }
        let frames = input.len() / channels;
        let skip = frames.saturating_sub(self.samples.capacity() / channels);
        if skip > 0 || self.end.is_some_and(|end| end != meta.frame_offset) {
            self.samples.clear();
            self.offset = 0;
            meta.frame_offset = meta
                .frame_offset
                .saturating_add(u64::try_from(skip).unwrap_or(u64::MAX));
            self.start =
                i64::try_from(meta.frame_offset).map_err(|_| ElasticError::SampleCountOverflow)?;
            self.end = Some(meta.frame_offset);
        }
        self.append(meta, &input[skip * channels..])
    }
}
