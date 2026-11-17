use std::collections::BTreeMap;

use kithara_blob::{BlobError, Reader, Writer};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_signal::{CoverageRead, CoverageWrite};
use rangemap::RangeSet;

use super::frames::{FrameGrid, Pending, frame_len, frame_size};
use crate::consts;

/// Estimator state that lets a stopped estimate continue without the frames
/// it already summed. A checkpoint of the estimator, read and validated
/// before any estimator takes it.
pub struct KeyResume {
    sum: [f64; consts::PITCH_CLASSES],
    end: Option<u64>,
    evaluated: RangeSet<u64>,
    pending: Vec<PendingResume>,
}

/// One frame part of whose samples had arrived.
struct PendingResume {
    index: u64,
    written: RangeSet<u64>,
    samples: Box<[f32]>,
}

impl KeyResume {
    /// Read the state [`crate::KeyEstimator::write_state`] wrote.
    ///
    /// # Errors
    ///
    /// [`BlobError::Version`] for state of another layout, and
    /// [`BlobError::Corrupt`] for a truncated state, a non-finite sum, or a
    /// pending frame that is out of order, mis-sized or already evaluated.
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, BlobError> {
        let sum = read_versioned_sum(reader)?;
        let end = reader.read_optional_u64()?;
        let evaluated = reader.read_coverage()?;
        let count = reader.read_count(PendingResume::MIN_BYTES)?;
        let mut pending: Vec<PendingResume> = Vec::with_capacity(Reader::capacity_for(count));
        let mut previous = None;
        for _ in 0..count {
            let frame = PendingResume::read(reader, &evaluated, previous)?;
            previous = Some(frame.index);
            pending.push(frame);
        }
        Ok(Self {
            sum,
            end,
            evaluated,
            pending,
        })
    }
}

impl PendingResume {
    /// An index, a coverage count and a sample count.
    const MIN_BYTES: usize = 3 * size_of::<u64>();

    fn read(
        reader: &mut Reader<'_>,
        evaluated: &RangeSet<u64>,
        previous: Option<u64>,
    ) -> Result<Self, BlobError> {
        let index = reader.read_ordered(previous)?;
        let written = reader.read_coverage()?;
        let samples = reader.read_samples()?;
        let fits = written.last().is_some_and(|run| run.end <= frame_size());
        if samples.len() != frame_len() || !fits || evaluated.contains(&index) {
            return Err(BlobError::Corrupt);
        }
        Ok(Self {
            index,
            written,
            samples,
        })
    }
}

impl FrameGrid {
    pub(crate) fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: KeyResume,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        let mut pending = BTreeMap::new();
        for frame in resume.pending {
            let mut samples = pools.get_with_len::<f32>(frame_len())?;
            samples.copy_from_slice(&frame.samples);
            pending.insert(
                frame.index,
                Pending {
                    written: frame.written,
                    samples,
                },
            );
        }
        self.totals.sum = resume.sum;
        self.totals.evaluated = resume.evaluated;
        self.end = resume.end;
        self.pending = pending;
        Ok(())
    }

    pub(crate) fn write_state(&self, out: &mut Vec<u8>) {
        let mut writer = Writer::new(out);
        writer.write_u32(consts::STATE_VERSION);
        for total in self.totals.sum {
            writer.write_f64(total);
        }
        writer.write_optional_u64(self.end);
        writer.write_coverage(&self.totals.evaluated);
        writer.write_len(self.pending.len());
        for (index, frame) in &self.pending {
            writer.write_u64(*index);
            writer.write_coverage(&frame.written);
            writer.write_samples(&frame.samples);
        }
    }
}

fn read_versioned_sum(reader: &mut Reader<'_>) -> Result<[f64; consts::PITCH_CLASSES], BlobError> {
    let found = reader.read_u32()?;
    if found != consts::STATE_VERSION {
        return Err(BlobError::Version {
            found,
            expected: consts::STATE_VERSION,
        });
    }
    let mut sum = [0.0; consts::PITCH_CLASSES];
    for total in &mut sum {
        *total = reader.read_f64()?;
    }
    if sum.iter().any(|total| !total.is_finite()) {
        return Err(BlobError::Corrupt);
    }
    Ok(sum)
}
