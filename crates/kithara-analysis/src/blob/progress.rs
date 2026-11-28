use crate::{
    AnalysisFingerprint, AnalysisProgress, TrackAnalysis,
    blob::{BlobError, Reader, Writer},
    progress::AnalysisResume,
};

mod consts {
    pub(super) const ANALYSIS_PROGRESS_BYTES_VERSION: u32 = 0x4b41_5001;
}

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
        let analysis = narrow(
            &TrackAnalysis::try_from(reader.read_section()?)?,
            fingerprint,
        )?;
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

/// The part of a stored snapshot the `active` analyzers still produce: an
/// artifact stored under another tag is dropped, and a snapshot left with
/// nothing is a miss, unless neither it nor `active` names any analyzer.
fn narrow(
    stored: &TrackAnalysis,
    active: &AnalysisFingerprint,
) -> Result<TrackAnalysis, BlobError> {
    let tags = stored.fingerprint();
    let waveform = served(tags.waveform(), active.waveform());
    let beat = served(tags.beat(), active.beat());
    let key = served(tags.key(), active.key());
    let nothing_served = waveform.is_none() && beat.is_none() && key.is_none();
    let blank = *tags == AnalysisFingerprint::default()
        && stored.waveform().is_none()
        && stored.beat().is_none()
        && stored.key().is_none();
    if nothing_served && !(blank && *active == AnalysisFingerprint::default()) {
        return Err(BlobError::Fingerprint);
    }
    Ok(TrackAnalysis::builder()
        .token(stored.token().clone())
        .revision(stored.revision())
        .source_sample_rate(stored.source_sample_rate())
        .maybe_media_origin(stored.media_origin())
        .maybe_extent(stored.extent())
        .settled(stored.is_settled())
        .coverage(stored.coverage().clone())
        .maybe_waveform(stored.waveform().filter(|_| waveform.is_some()).cloned())
        .maybe_beat(stored.beat().filter(|_| beat.is_some()).cloned())
        .maybe_key(stored.key().filter(|_| key.is_some()))
        .fingerprint(
            AnalysisFingerprint::builder()
                .maybe_waveform(waveform)
                .maybe_beat(beat)
                .maybe_key(key)
                .build(),
        )
        .build())
}

/// The stored tag when `active` is the configuration that wrote it.
fn served<'a>(stored: Option<&'a str>, active: Option<&str>) -> Option<&'a str> {
    stored.filter(|tag| active == Some(*tag))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use kithara_key::KeyEstimator;
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use rangemap::RangeSet;

    use super::*;
    use crate::{
        AnalysisToken, BeatArtifact, BeatSnapshot, BeatState, Bucket, KeyEstimate, Mode,
        MusicalKey, PitchClass, Waveform,
    };

    fn analysis(settled: bool) -> TrackAnalysis {
        TrackAnalysis::builder()
            .media_origin(Duration::ZERO)
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
            Some(AnalysisResume::capture(NonZeroU64::MIN, None, None, None)),
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

        for (name, runs, windows, valid) in [
            ("valid beat state", &[(0, 2), (3, 4)][..], &[0, 1][..], true),
            ("overlapping runs", &[(0, 2), (1, 4)][..], &[][..], false),
            ("empty run", &[(2, 2)][..], &[][..], false),
            ("descending windows", &[][..], &[1, 0][..], false),
        ] {
            let mut beat = Vec::new();
            let mut writer = Writer::new(&mut beat);
            writer.write_bool(false);
            writer.write_len(runs.len());
            for &(start, end) in runs {
                writer.write_u64(start);
                writer.write_u64(end);
                writer.write_samples(&[0.5]);
            }
            writer.write_len(1);
            writer.write_u64(0);
            writer.write_u64(8);
            writer.write_len(windows.len());
            for &index in windows {
                writer.write_u64(index);
                writer.write_len(0);
                writer.write_len(0);
            }
            writer.write_len(0);

            let progress = AnalysisProgress::new(
                analysis(false),
                Some(AnalysisResume::capture(
                    NonZeroU64::MIN,
                    None,
                    Some(&beat),
                    None,
                )),
            );
            let mut bytes = Vec::new();
            progress.write_to(&mut bytes).expect("encodes");
            let decoded =
                AnalysisProgress::try_from((bytes.as_slice(), &AnalysisFingerprint::default()));
            if valid {
                assert!(decoded.is_ok(), "{name}: {decoded:?}");
            } else {
                assert!(matches!(decoded, Err(BlobError::Corrupt)), "{name}");
            }
        }
    }

    #[kithara::test]
    fn a_damaged_key_resume_section_is_a_miss() {
        let pools = kithara_test_utils::bufpool::pools();
        let estimator = KeyEstimator::new(44_100, &pools).expect("the estimator fits");
        let mut state = Vec::new();
        estimator.write_state(&mut state);
        let mut nan_sum = state.clone();
        nan_sum[4..12].copy_from_slice(&f64::NAN.to_le_bytes());
        let mut foreign = state;
        let version = u32::from_le_bytes([foreign[0], foreign[1], foreign[2], foreign[3]]);
        foreign[..4].copy_from_slice(&(version + 1).to_le_bytes());

        for (name, key) in [("a non-finite sum", nan_sum), ("another version", foreign)] {
            let progress = AnalysisProgress::new(
                analysis(false),
                Some(AnalysisResume::capture(
                    NonZeroU64::MIN,
                    None,
                    None,
                    Some(&key),
                )),
            );
            let mut bytes = Vec::new();
            progress.write_to(&mut bytes).expect("encodes");
            let decoded =
                AnalysisProgress::try_from((bytes.as_slice(), &AnalysisFingerprint::default()));
            assert!(
                matches!(decoded, Err(BlobError::Corrupt | BlobError::Version { .. })),
                "{name}: a checkpoint whose key state is unreadable is a miss, got {decoded:?}"
            );
        }
    }

    fn beat() -> BeatArtifact {
        BeatArtifact::new(
            128.0,
            vec![(0, Some(0.9)), (10_000, Some(0.75))],
            Vec::new(),
        )
    }

    fn wave() -> Waveform {
        Waveform::try_new(vec![Bucket::new([0.5; 4], [0.5; 4])], NonZeroU64::MIN)
            .expect("hand-built buckets are valid")
    }

    fn tagged(wave: &str, beat: &str, key: &str) -> AnalysisFingerprint {
        AnalysisFingerprint::builder()
            .waveform(wave)
            .beat(beat)
            .key(key)
            .build()
    }

    fn stored(tags: AnalysisFingerprint, key: KeyEstimate) -> TrackAnalysis {
        TrackAnalysis::builder()
            .token(crate::consts::TRACK_TOKEN.into())
            .revision(3)
            .source_sample_rate(std::num::NonZeroU32::MIN)
            .extent(1_000)
            .settled(true)
            .fingerprint(tags)
            .waveform(wave())
            .beat(BeatSnapshot::new(beat(), BeatState::Final, Vec::new()))
            .key(key)
            .build()
    }

    /// A settled snapshot as the local cache stores and reads it back.
    fn cached(
        analysis: TrackAnalysis,
        active: &AnalysisFingerprint,
    ) -> Result<TrackAnalysis, BlobError> {
        let mut bytes = Vec::new();
        AnalysisProgress::try_from(analysis)?.write_to(&mut bytes)?;
        AnalysisProgress::try_from((bytes.as_slice(), active)).map(TrackAnalysis::from)
    }

    fn a_minor() -> KeyEstimate {
        KeyEstimate::new(
            MusicalKey {
                tonic: PitchClass::A,
                mode: Mode::Minor,
            },
            0.875,
        )
        .expect("a fixture strength is in range")
    }

    #[kithara::test]
    fn stale_fingerprint_is_a_miss() {
        let tags = tagged("wave:fold1:v2", crate::consts::BEAT_TAG, "key:test:v1");
        assert!(matches!(
            cached(
                stored(tags, a_minor()),
                &tagged("other-wave", "other-beat", "other-key")
            ),
            Err(BlobError::Fingerprint)
        ));
    }

    #[kithara::test]
    fn a_foreign_beat_tag_leaves_the_matching_waveform() {
        let got = cached(
            stored(
                tagged("wave:fold1:v2", "beat:another-model:v1", "key:test:v1"),
                a_minor(),
            ),
            &tagged("wave:fold1:v2", crate::consts::BEAT_TAG, "key:test:v1"),
        )
        .expect("the waveform and key tags still match");

        assert_eq!(got.waveform(), Some(&wave()));
        assert!(got.beat().is_none(), "another beat model is a beat miss");
        assert_eq!(got.fingerprint().beat(), None);
        assert_eq!(got.fingerprint().waveform(), Some("wave:fold1:v2"));
    }

    #[kithara::test]
    fn a_key_round_trips_under_its_own_tag() {
        let tags = |key: &str| tagged("wave:fold1:v2", crate::consts::BEAT_TAG, key);
        let want = stored(tags("key:test:v1"), a_minor());

        let got = cached(want.clone(), want.fingerprint()).expect("decodes");
        assert_eq!(got.key(), Some(a_minor()));
        assert_eq!(got.fingerprint(), want.fingerprint());

        let other =
            cached(want, &tags("key:other:v1")).expect("the beat and waveform tags still match");
        assert_eq!(other.key(), None, "another key configuration is a key miss");
        assert_eq!(other.fingerprint().key(), None);
        assert_eq!(other.fingerprint().beat(), Some(crate::consts::BEAT_TAG));
        assert_eq!(
            other.beat().map(BeatSnapshot::artifact),
            Some(&beat()),
            "the beat result stays usable"
        );
    }
}
