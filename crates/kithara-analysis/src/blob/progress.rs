use crate::{
    AnalysisFingerprint, AnalysisProgress, TrackAnalysis,
    blob::{BlobError, Reader, Writer},
    consts,
    progress::AnalysisResume,
};

impl AnalysisProgress {
    pub(crate) fn write_to(&self, out: &mut Vec<u8>) -> Result<(), BlobError> {
        let mut analysis = Vec::new();
        self.analysis().write_to(&mut analysis)?;

        let mut writer = Writer::new(out);
        writer.write_u32(consts::ANALYSIS_PROGRESS_BYTES_VERSION);
        writer.write_len(analysis.len());
        writer.write_bytes(&analysis);
        let resume = self.resume().map(AnalysisResume::bytes).unwrap_or_default();
        writer.write_len(resume.len());
        writer.write_bytes(resume);
        Ok(())
    }
}

impl TryFrom<(&[u8], &AnalysisFingerprint)> for AnalysisProgress {
    type Error = BlobError;

    fn try_from((bytes, fingerprint): (&[u8], &AnalysisFingerprint)) -> Result<Self, Self::Error> {
        let mut reader = Reader::new(bytes);
        let version = reader.read_u32()?;
        if version != consts::ANALYSIS_PROGRESS_BYTES_VERSION {
            return Err(BlobError::Version {
                found: version,
                expected: consts::ANALYSIS_PROGRESS_BYTES_VERSION,
            });
        }
        let analysis = TrackAnalysis::try_from((reader.read_section()?, fingerprint))?;
        let resume = match reader.read_section()? {
            [] => None,
            bytes => Some(AnalysisResume::try_from(bytes)?),
        };
        reader.finish()?;
        let progress = Self::new(analysis, resume);
        progress.validate_resume()?;
        Ok(progress)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use kithara_test_utils::kithara;
    use rangemap::RangeSet;

    use super::*;
    use crate::AnalysisToken;

    fn analysis(settled: bool) -> TrackAnalysis {
        TrackAnalysis::builder()
            .token(AnalysisToken::from("progress-codec"))
            .revision(7)
            .source_sample_rate(std::num::NonZeroU32::MIN)
            .extent(64)
            .coverage(RangeSet::new())
            .fingerprint(AnalysisFingerprint::default())
            .settled(settled)
            .build()
    }

    fn resumable() -> AnalysisProgress {
        AnalysisProgress::new(
            analysis(false),
            Some(AnalysisResume::capture(NonZeroU64::MIN, None, None)),
        )
    }

    #[kithara::test]
    fn codec_round_trips_settled_and_resumable_progress() {
        for want in [AnalysisProgress::new(analysis(true), None), resumable()] {
            let mut bytes = Vec::new();
            want.write_to(&mut bytes).expect("encodes");
            let got =
                AnalysisProgress::try_from((bytes.as_slice(), &AnalysisFingerprint::default()))
                    .expect("decodes");
            assert_eq!(got.analysis().revision(), want.analysis().revision());
            assert_eq!(got.is_resumable(), want.is_resumable());
            assert_eq!(got.analysis().is_settled(), want.analysis().is_settled());
        }
    }

    #[kithara::test]
    fn codec_rejects_wrong_version_truncation_and_trailing_bytes() {
        let mut bytes = Vec::new();
        resumable().write_to(&mut bytes).expect("encodes");

        let mut wrong = bytes.clone();
        wrong[0] = wrong[0].wrapping_add(1);
        assert!(matches!(
            AnalysisProgress::try_from((wrong.as_slice(), &AnalysisFingerprint::default())),
            Err(BlobError::Version { .. })
        ));
        assert!(matches!(
            AnalysisProgress::try_from((
                &bytes[..bytes.len().saturating_sub(1)],
                &AnalysisFingerprint::default(),
            )),
            Err(BlobError::Corrupt)
        ));

        bytes.push(0xff);
        assert!(matches!(
            AnalysisProgress::try_from((bytes.as_slice(), &AnalysisFingerprint::default())),
            Err(BlobError::Corrupt)
        ));
    }
}
