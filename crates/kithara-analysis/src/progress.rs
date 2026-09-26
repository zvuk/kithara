use std::{collections::BTreeSet, num::NonZeroU64};

use kithara_platform::sync::Arc;
use kithara_signal::{CoverageRead, FrameCoverage};
use kithara_waveform::WaveformResume;
use rangemap::RangeSet;

use crate::{
    BlobError, TrackAnalysis,
    blob::{MAX_PREALLOC, Reader, Writer},
    consts,
};

/// One atomic analysis publication and the opaque state needed to continue it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AnalysisProgress {
    resume: Option<AnalysisResume>,
    analysis: TrackAnalysis,
}

impl AnalysisProgress {
    pub(crate) const fn new(analysis: TrackAnalysis, resume: Option<AnalysisResume>) -> Self {
        Self { resume, analysis }
    }

    /// Renderable analysis snapshot, kept separate from the potentially large
    /// resume payload so UI state can clone only this value.
    #[must_use]
    pub const fn analysis(&self) -> &TrackAnalysis {
        &self.analysis
    }

    pub(crate) fn decode_resume(&self) -> Result<Option<ResumeState>, BlobError> {
        self.validate_resume()?;
        self.resume.as_ref().map(AnalysisResume::decode).transpose()
    }

    pub(crate) fn resume_meta(&self) -> Option<(NonZeroU64, (bool, bool))> {
        self.resume
            .as_ref()
            .map(|resume| (resume.chunk_frames, resume.shape))
    }

    pub(crate) fn validate_resume(&self) -> Result<(), BlobError> {
        match (self.analysis.is_settled(), &self.resume) {
            (true, None) | (false, Some(_)) => Ok(()),
            (true, Some(_)) | (false, None) => Err(BlobError::Corrupt),
        }
    }

    delegate::delegate! {
        to self.resume {
            /// Whether this publication contains validated analyzer state that can be
            /// resumed without decoding its covered source ranges again.
            #[must_use]
            #[call(is_some)]
            pub const fn is_resumable(&self) -> bool;
            #[call(as_ref)]
            pub(crate) const fn resume(&self) -> Option<&AnalysisResume>;
        }
    }
}

impl TryFrom<TrackAnalysis> for AnalysisProgress {
    type Error = BlobError;

    fn try_from(analysis: TrackAnalysis) -> Result<Self, Self::Error> {
        if !analysis.is_settled() {
            return Err(BlobError::Corrupt);
        }
        Ok(Self::new(analysis, None))
    }
}

impl From<AnalysisProgress> for TrackAnalysis {
    fn from(progress: AnalysisProgress) -> Self {
        progress.analysis
    }
}

#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[derive(derive_more::Debug)]
pub(crate) struct AnalysisResume {
    shape: (bool, bool),
    #[field(get, vis = "pub(crate)")]
    #[debug("{:?}", self.bytes.len())]
    bytes: Arc<[u8]>,
    chunk_frames: NonZeroU64,
}

impl AnalysisResume {
    pub(crate) fn capture(
        chunk_frames: NonZeroU64,
        waveform: Option<&[u8]>,
        beat: Option<&[u8]>,
    ) -> Self {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes);
        writer.write_u32(consts::RESUME_VERSION);
        writer.write_u64(chunk_frames.get());
        write_section(&mut writer, waveform);
        write_section(&mut writer, beat);
        Self {
            chunk_frames,
            bytes: Arc::from(bytes),
            shape: (waveform.is_some(), beat.is_some()),
        }
    }

    pub(crate) fn decode(&self) -> Result<ResumeState, BlobError> {
        decode_resume(&self.bytes)
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self, BlobError> {
        let bytes: Arc<[u8]> = Arc::from(bytes);
        let state = decode_resume(&bytes)?;
        Ok(Self {
            bytes,
            chunk_frames: state.chunk_frames,
            shape: (state.waveform.is_some(), state.beat.is_some()),
        })
    }
}

impl TryFrom<&[u8]> for AnalysisResume {
    type Error = BlobError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::decode_bytes(bytes)
    }
}

pub(crate) struct ResumeState {
    pub(crate) chunk_frames: NonZeroU64,
    pub(crate) beat: Option<BeatResume>,
    pub(crate) waveform: Option<WaveformResume>,
}

pub(crate) struct BeatResume {
    pub(crate) short: BTreeSet<usize>,
    pub(crate) taken: RangeSet<u64>,
    pub(crate) runs: Vec<BeatRunResume>,
    pub(crate) windows: Vec<(usize, RawBeatsResume)>,
}

pub(crate) struct BeatRunResume {
    pub(crate) mono: Box<[f32]>,
    pub(crate) end: u64,
    pub(crate) start: u64,
}

pub(crate) struct RawBeatsResume {
    pub(crate) beats: Vec<BeatMarkResume>,
    pub(crate) downbeats: Vec<BeatMarkResume>,
}

#[derive(Clone, Copy)]
pub(crate) struct BeatMarkResume {
    pub(crate) at: f32,
    pub(crate) confidence: f32,
}

impl BeatResume {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, BlobError> {
        let run_count = reader.read_count(24)?;
        let mut runs: Vec<BeatRunResume> = Vec::with_capacity(run_count.min(MAX_PREALLOC));
        let mut previous_end = None;
        for _ in 0..run_count {
            let start = reader.read_u64()?;
            let end = reader.read_u64()?;
            if start >= end || previous_end.is_some_and(|previous| previous >= start) {
                return Err(BlobError::Corrupt);
            }
            previous_end = Some(end);
            runs.push(BeatRunResume {
                start,
                end,
                mono: reader.read_samples()?,
            });
        }

        let taken = reader.read_coverage()?;

        let window_count = reader.read_count(24)?;
        let mut windows: Vec<(usize, RawBeatsResume)> =
            Vec::with_capacity(window_count.min(MAX_PREALLOC));
        let mut previous = None;
        for _ in 0..window_count {
            let raw = reader.read_ordered(previous)?;
            let index = usize::try_from(raw).map_err(|_| BlobError::Corrupt)?;
            previous = Some(raw);
            windows.push((
                index,
                RawBeatsResume {
                    beats: read_marks(reader)?,
                    downbeats: read_marks(reader)?,
                },
            ));
        }

        let short_count = reader.read_count(8)?;
        let mut short = BTreeSet::new();
        previous = None;
        for _ in 0..short_count {
            let raw = reader.read_ordered(previous)?;
            let index = usize::try_from(raw).map_err(|_| BlobError::Corrupt)?;
            previous = Some(raw);
            short.insert(index);
        }
        let resume = Self {
            short,
            taken,
            runs,
            windows,
        };
        resume.validate()?;
        Ok(resume)
    }

    fn validate(&self) -> Result<(), BlobError> {
        if self.runs.iter().any(|run| run.mono.is_empty())
            || self
                .runs
                .iter()
                .any(|run| !self.taken.covers(&(run.start..run.end)))
            || self.short.iter().any(|index| {
                self.windows
                    .binary_search_by_key(index, |(at, _)| *at)
                    .is_err()
            })
            || self.windows.iter().any(|(_, raw)| {
                raw.beats
                    .iter()
                    .chain(&raw.downbeats)
                    .any(|mark| !mark.at.is_finite() || !mark.confidence.is_finite())
            })
        {
            return Err(BlobError::Corrupt);
        }
        Ok(())
    }
}

fn write_section(writer: &mut Writer<'_>, bytes: Option<&[u8]>) {
    let bytes = bytes.unwrap_or_default();
    writer.write_len(bytes.len());
    writer.write_bytes(bytes);
}

fn decode_resume(bytes: &[u8]) -> Result<ResumeState, BlobError> {
    let mut reader = Reader::new(bytes);
    let version = reader.read_u32()?;
    if version != consts::RESUME_VERSION {
        return Err(BlobError::Version {
            found: version,
            expected: consts::RESUME_VERSION,
        });
    }
    let chunk_frames = NonZeroU64::new(reader.read_u64()?).ok_or(BlobError::Corrupt)?;
    let waveform = decode_section(&mut reader, WaveformResume::decode)?;
    let beat = decode_section(&mut reader, BeatResume::decode)?;
    reader.finish()?;
    Ok(ResumeState {
        chunk_frames,
        beat,
        waveform,
    })
}

fn decode_section<T>(
    reader: &mut Reader<'_>,
    decode: impl FnOnce(&mut Reader<'_>) -> Result<T, BlobError>,
) -> Result<Option<T>, BlobError> {
    let bytes = reader.read_section()?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let mut section = Reader::new(bytes);
    let value = decode(&mut section)?;
    section.finish()?;
    Ok(Some(value))
}

fn read_marks(reader: &mut Reader<'_>) -> Result<Vec<BeatMarkResume>, BlobError> {
    let count = reader.read_count(8)?;
    let mut marks: Vec<BeatMarkResume> = Vec::with_capacity(count.min(MAX_PREALLOC));
    for _ in 0..count {
        let at = reader.read_f32()?;
        let confidence = reader.read_f32()?;
        if !at.is_finite() || at < 0.0 || !confidence.is_finite() {
            return Err(BlobError::Corrupt);
        }
        marks.push(BeatMarkResume { at, confidence });
    }
    Ok(marks)
}
