use kithara_blob::{BlobError, Reader};
use kithara_signal::CoverageRead;
use rangemap::RangeSet;

/// Analyzer state that lets a stopped waveform pass continue without decoding
/// the source ranges it already reduced. This is a checkpoint of the algorithm,
/// not a waveform result: a served waveform never carries one.
pub struct WaveformResume {
    pub(crate) columns: Vec<(u64, [[u8; 8]; crate::consts::COLUMNS])>,
    pub(crate) partials: Vec<WaveformPartialResume>,
    pub(crate) opened: u64,
}

/// One window whose span is not yet inside a single covered run.
pub struct WaveformPartialResume {
    pub(crate) samples: Box<[f32]>,
    pub(crate) written: RangeSet<u64>,
    pub(crate) index: u64,
    pub(crate) seq: u64,
}

impl WaveformResume {
    /// Read the waveform section of a resume record.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Corrupt`] for a truncated section, an out-of-order index, or a partial that contradicts the
    /// bands beside it.
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, BlobError> {
        let count = reader.read_count(8 + 48 * 8)?;
        let mut columns: Vec<(u64, [[u8; 8]; crate::consts::COLUMNS])> =
            Vec::with_capacity(Reader::capacity_for(count));
        let mut previous = None;
        for _ in 0..count {
            let index = reader.read_ordered(previous)?;
            previous = Some(index);
            let mut values = [[0; 8]; crate::consts::COLUMNS];
            for column in &mut values {
                *column = reader.read_array()?;
            }
            columns.push((index, values));
        }

        let partial_count = reader.read_count(32)?;
        let mut partials: Vec<WaveformPartialResume> =
            Vec::with_capacity(Reader::capacity_for(partial_count));
        previous = None;
        for _ in 0..partial_count {
            let index = reader.read_ordered(previous)?;
            previous = Some(index);
            partials.push(WaveformPartialResume {
                index,
                samples: reader.read_samples()?,
                written: reader.read_coverage()?,
                seq: reader.read_u64()?,
            });
        }
        let opened = reader.read_u64()?;
        let resume = Self {
            columns,
            partials,
            opened,
        };
        resume.validate()?;
        Ok(resume)
    }

    fn validate(&self) -> Result<(), BlobError> {
        let mut partials = self.partials.iter();
        let mut partial = partials.next();
        for (index, _) in &self.columns {
            while partial.is_some_and(|held| held.index < *index) {
                partial = partials.next();
            }
            if partial.is_some_and(|held| held.index == *index) {
                return Err(BlobError::Corrupt);
            }
        }
        if self.partials.iter().any(|held| {
            held.samples.is_empty()
                || held.written.iter().next().is_none()
                || held.seq >= self.opened
        }) {
            return Err(BlobError::Corrupt);
        }
        Ok(())
    }
}
