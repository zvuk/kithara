use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicU32, AtomicU64, Ordering, fence},
};

use kithara_decode::{DecodeError, DecodeResult, PcmSpec};
use kithara_platform::sync::Arc;

use crate::traits::PresentationPoint;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub(crate) enum PresentationBarrier {
    DecoderReplaced { epoch: u64, spec: PcmSpec },
}

impl PresentationBarrier {
    pub(crate) const fn epoch(self) -> u64 {
        match self {
            Self::DecoderReplaced { epoch, .. } => epoch,
        }
    }
}

struct FrontierInner {
    epoch: AtomicU64,
    frame: AtomicU64,
    generation: AtomicU64,
    output_end: AtomicU64,
    rate: AtomicU32,
    revision: AtomicU64,
}

/// Read-only coherent endpoint of PCM committed to final output.
#[derive(Clone)]
pub(crate) struct PresentationFrontier {
    inner: Arc<FrontierInner>,
}

/// Sole writer for one resource's presentation frontier.
pub(crate) struct PresentationPublisher {
    frame: u64,
    generation: u64,
    inner: Arc<FrontierInner>,
    output_end: u64,
    rate: u32,
}

impl PresentationFrontier {
    pub(crate) fn point(&self, epoch: u64) -> Option<PresentationPoint> {
        let point = self.snapshot()?;
        (point.seek_epoch() == epoch).then_some(point)
    }

    pub(super) fn snapshot(&self) -> Option<PresentationPoint> {
        let before = self.inner.revision.load(Ordering::Acquire);
        if before & 1 != 0 {
            return None;
        }
        let rate = NonZeroU32::new(self.inner.rate.load(Ordering::Relaxed))?;
        let point = PresentationPoint::new(
            self.inner.epoch.load(Ordering::Relaxed),
            self.inner.frame.load(Ordering::Relaxed),
            self.inner.generation.load(Ordering::Relaxed),
            self.inner.output_end.load(Ordering::Relaxed),
            rate,
        );
        fence(Ordering::Acquire);
        let after = self.inner.revision.load(Ordering::Acquire);
        (before == after).then_some(point)
    }
}

impl PresentationPublisher {
    fn publish(&mut self, point: Option<PresentationPoint>) {
        let start = self.inner.revision.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(start & 1, 0, "presentation frontier has one writer");
        if let Some(point) = point {
            self.inner
                .epoch
                .store(point.seek_epoch(), Ordering::Relaxed);
            self.inner
                .frame
                .store(point.source_frame(), Ordering::Relaxed);
            self.inner
                .generation
                .store(point.generation(), Ordering::Relaxed);
            self.inner
                .output_end
                .store(point.output_end(), Ordering::Relaxed);
            self.inner
                .rate
                .store(point.sample_rate().get(), Ordering::Relaxed);
        } else {
            self.inner.rate.store(0, Ordering::Relaxed);
        }
        self.inner
            .revision
            .store(start.wrapping_add(2), Ordering::Release);
    }

    pub(super) fn prepare_commit(
        &self,
        epoch: u64,
        output_frames: usize,
        source_end: Option<(u64, u32)>,
    ) -> DecodeResult<PresentationPoint> {
        let output_frames = u64::try_from(output_frames).map_err(|_| DecodeError::InvalidData {
            detail: "presentation output block length exceeds u64",
        })?;
        let output_end =
            self.output_end
                .checked_add(output_frames)
                .ok_or(DecodeError::InvalidData {
                    detail: "presentation output ordinal overflow",
                })?;
        let (frame, rate) = source_end.unwrap_or((self.frame, self.rate));
        let rate = NonZeroU32::new(rate).ok_or(DecodeError::InvalidData {
            detail: "presentation source sample rate is unavailable",
        })?;
        Ok(PresentationPoint::new(
            epoch,
            frame,
            self.generation,
            output_end,
            rate,
        ))
    }

    pub(super) fn commit(&mut self, point: PresentationPoint) {
        self.frame = point.source_frame();
        self.output_end = point.output_end();
        self.rate = point.sample_rate().get();
        self.publish(Some(point));
    }

    pub(super) fn point(&self, epoch: u64) -> Option<PresentationPoint> {
        let rate = NonZeroU32::new(self.rate)?;
        Some(PresentationPoint::new(
            epoch,
            self.frame,
            self.generation,
            self.output_end,
            rate,
        ))
    }

    pub(super) fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.frame = 0;
        self.output_end = 0;
        self.rate = 0;
        self.publish(None);
    }

    pub(super) fn restart(&mut self, epoch: u64, source_end: Option<(u64, u32)>) {
        self.generation = self.generation.wrapping_add(1);
        if let Some((frame, rate)) = source_end {
            self.frame = frame;
            self.rate = rate;
        }
        self.output_end = 0;
        let point = NonZeroU32::new(self.rate)
            .map(|rate| PresentationPoint::new(epoch, self.frame, self.generation, 0, rate));
        self.publish(point);
    }
}

pub(crate) fn presentation_cell(epoch: u64) -> (PresentationPublisher, PresentationFrontier) {
    let inner = Arc::new(FrontierInner {
        epoch: AtomicU64::new(epoch),
        frame: AtomicU64::new(0),
        generation: AtomicU64::new(0),
        output_end: AtomicU64::new(0),
        rate: AtomicU32::new(0),
        revision: AtomicU64::new(0),
    });
    (
        PresentationPublisher {
            frame: 0,
            generation: 0,
            inner: Arc::clone(&inner),
            output_end: 0,
            rate: 0,
        },
        PresentationFrontier { inner },
    )
}
