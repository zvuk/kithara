use std::num::NonZeroU64;

use kithara_bufpool::{RingCons, SampleBuffer};
use kithara_platform::sync::{Arc, atomic::Ordering};
use kithara_worker::{Task, TickResult};
use ringbuf::{
    HeapCons,
    traits::{Consumer, Observer},
};

use super::{FormatChange, LiveRecordingReport, recorder::Control};
use crate::{
    LiveRecordingConfig, LiveRecordingError, PartSinkFactory, RecordingConfig, RecordingCore,
    consts,
};

pub(super) struct RecordingTask<F>
where
    F: PartSinkFactory,
{
    control: Arc<Control>,
    factory: F,
    formats: HeapCons<FormatChange>,
    pcm: RingCons<SampleBuffer>,
    core: Option<RecordingCore<F::Sink>>,
    next_format: Option<FormatChange>,
    rotation_frames: Option<NonZeroU64>,
    scratch: Option<SampleBuffer>,
    config: RecordingConfig,
    frames: u64,
    part_frames: u64,
    parts: u64,
    buffer_frames: usize,
    generation_capacity: usize,
}

impl<F> RecordingTask<F>
where
    F: PartSinkFactory,
{
    pub(super) fn new<S>(
        config: LiveRecordingConfig<F, S>,
        pcm: RingCons<SampleBuffer>,
        formats: HeapCons<FormatChange>,
        control: Arc<Control>,
        buffer_frames: usize,
        scratch: SampleBuffer,
    ) -> Self {
        let generation_capacity = config.generation_capacity.get();
        let rotation_frames = config.rotation_frames;
        let recording = config.recording;
        let factory = config.factory;
        Self {
            buffer_frames,
            control,
            factory,
            formats,
            generation_capacity,
            pcm,
            rotation_frames,
            config: recording,
            core: None,
            frames: 0,
            next_format: None,
            part_frames: 0,
            parts: 0,
            scratch: Some(scratch),
        }
    }

    fn apply_format(&mut self, change: FormatChange) -> Result<(), LiveRecordingError> {
        if change.spec.channels != consts::CHANNELS {
            return Err(LiveRecordingError::ChannelCount(change.spec.channels));
        }
        self.finish_part()?;
        self.config.set_sample_rate(change.spec.sample_rate.get());
        self.next_format = None;
        Ok(())
    }

    fn clear_cut(&self, at: u64) {
        self.control
            .cut_at
            .compare_exchange(at, consts::NO_CUT, Ordering::AcqRel, Ordering::Relaxed)
            .ok();
    }

    fn fail(&mut self, error: LiveRecordingError) -> TickResult {
        self.control.accepting.store(false, Ordering::Release);
        self.core.take();
        self.publish(Err(error));
        TickResult::Done
    }

    fn finish_part(&mut self) -> Result<(), LiveRecordingError> {
        if self.part_frames == 0 {
            return Ok(());
        }
        let part = self
            .parts
            .checked_add(1)
            .ok_or(LiveRecordingError::FrameCountOverflow)?;
        let Some(core) = self.core.take() else {
            return Err(LiveRecordingError::FrameCountOverflow);
        };
        core.finish().map_err(|source| LiveRecordingError::Part {
            part,
            source: Box::new(source),
        })?;
        self.parts = part;
        self.part_frames = 0;
        Ok(())
    }

    fn next_format(&mut self) -> Option<FormatChange> {
        if self.next_format.is_none() {
            self.next_format = self.formats.try_pop();
        }
        self.next_format
    }

    fn open_part(&mut self) -> Result<(), LiveRecordingError> {
        if self.core.is_some() {
            return Ok(());
        }
        let part = self
            .parts
            .checked_add(1)
            .ok_or(LiveRecordingError::FrameCountOverflow)?;
        let sink = self
            .factory
            .open(part)
            .map_err(|source| LiveRecordingError::OpenPart {
                part,
                source: Box::new(source),
            })?;
        let core = RecordingCore::new(&self.config, sink, None).map_err(|source| {
            LiveRecordingError::Part {
                part,
                source: Box::new(source),
            }
        })?;
        self.core = Some(core);
        Ok(())
    }

    fn process(&mut self, samples: &[f32]) -> Result<(), LiveRecordingError> {
        let mut sample = 0;
        loop {
            if let Some(change) = self.next_format()
                && change.frame <= self.frames
            {
                self.apply_format(change)?;
                continue;
            }
            let cut_at = self.control.cut_at.load(Ordering::Acquire);
            if cut_at <= self.frames {
                self.finish_part()?;
                self.clear_cut(cut_at);
                continue;
            }
            if sample >= samples.len() {
                break;
            }
            let available_frames = (samples.len() - sample) / consts::STEREO;
            let mut take = u64::try_from(available_frames)
                .map_err(|_| LiveRecordingError::FrameCountOverflow)?;
            if cut_at != consts::NO_CUT {
                take = take.min(cut_at - self.frames);
            }
            if let Some(change) = self.next_format() {
                take = take.min(change.frame - self.frames);
            }
            if let Some(rotation) = self.rotation_frames {
                take = take.min(rotation.get() - self.part_frames);
            }
            let take = usize::try_from(take).map_err(|_| LiveRecordingError::FrameCountOverflow)?;
            let end = sample
                .checked_add(
                    take.checked_mul(consts::STEREO)
                        .ok_or(LiveRecordingError::FrameCountOverflow)?,
                )
                .ok_or(LiveRecordingError::FrameCountOverflow)?;
            self.push(&samples[sample..end])?;
            sample = end;

            let cut_at = self.control.cut_at.load(Ordering::Acquire);
            if cut_at == self.frames {
                self.finish_part()?;
                self.clear_cut(cut_at);
            } else if self
                .rotation_frames
                .is_some_and(|rotation| self.part_frames == rotation.get())
            {
                self.finish_part()?;
            }
        }
        Ok(())
    }

    fn publish(&self, result: Result<LiveRecordingReport, LiveRecordingError>) {
        let mut slot = self.control.result.lock();
        if slot.is_none() {
            *slot = Some(result);
        }
    }

    fn push(&mut self, samples: &[f32]) -> Result<(), LiveRecordingError> {
        let frames = u64::try_from(samples.len() / consts::STEREO)
            .map_err(|_| LiveRecordingError::FrameCountOverflow)?;
        self.open_part()?;
        let part = self
            .parts
            .checked_add(1)
            .ok_or(LiveRecordingError::FrameCountOverflow)?;
        self.core
            .as_mut()
            .ok_or(LiveRecordingError::FrameCountOverflow)?
            .push(samples)
            .map_err(|source| LiveRecordingError::Part {
                part,
                source: Box::new(source),
            })?;
        self.frames = self
            .frames
            .checked_add(frames)
            .ok_or(LiveRecordingError::FrameCountOverflow)?;
        self.part_frames = self
            .part_frames
            .checked_add(frames)
            .ok_or(LiveRecordingError::FrameCountOverflow)?;
        Ok(())
    }
}

impl<F> Task for RecordingTask<F>
where
    F: PartSinkFactory,
{
    fn on_cancel(&mut self) {
        self.core.take();
        self.publish(Err(LiveRecordingError::Cancelled));
    }

    fn tick(&mut self) -> TickResult {
        if self.control.generation_overflowed.load(Ordering::Acquire) {
            return self.fail(LiveRecordingError::GenerationQueueOverflow {
                capacity: self.generation_capacity,
            });
        }
        if self.control.overflowed.load(Ordering::Acquire) {
            return self.fail(LiveRecordingError::BufferOverflow {
                buffer_frames: self.buffer_frames,
            });
        }

        let Some(mut scratch) = self.scratch.take() else {
            return self.fail(LiveRecordingError::Cancelled);
        };
        let samples = self.pcm.occupied_len().min(scratch.len());
        let samples = samples - samples % consts::STEREO;
        let taken = self.pcm.pop_slice(&mut scratch[..samples]);
        let processed = self.process(&scratch[..taken]);
        self.scratch = Some(scratch);
        if let Err(error) = processed {
            return self.fail(error);
        }

        let finished = self.control.finish_requested.load(Ordering::Acquire)
            && !self.control.writing.load(Ordering::Acquire)
            && self.pcm.is_empty();
        if finished {
            if self.control.overflowed.load(Ordering::Acquire) {
                return self.fail(LiveRecordingError::BufferOverflow {
                    buffer_frames: self.buffer_frames,
                });
            }
            if let Err(error) = self.finish_part() {
                return self.fail(error);
            }
            self.publish(Ok(LiveRecordingReport {
                frames: self.frames,
                parts: self.parts,
            }));
            return TickResult::Done;
        }
        if taken > 0 {
            TickResult::Progress
        } else {
            TickResult::Waiting
        }
    }
}
