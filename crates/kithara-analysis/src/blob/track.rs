use std::{num::NonZeroU32, ops::Range};

use kithara_key::{KeyEstimate, Mode, MusicalKey, PitchClass};
use kithara_platform::time::Duration;
use kithara_signal::{CoverageRead, CoverageWrite, FrameSpan};
use rangemap::RangeSet;

use crate::{
    AnalysisFingerprint, BeatArtifact, BeatSnapshot, BeatState, TrackAnalysis, Waveform,
    blob::{BlobError, MAX_PREALLOC, Reader, Writer},
    consts,
};

impl TrackAnalysis {
    /// Append this snapshot to caller-owned storage using the durable analysis format.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::TooLarge`] when a length does not fit the format.
    pub fn write_to(&self, out: &mut Vec<u8>) -> Result<(), BlobError> {
        let mut writer = Writer::new(out);
        writer.write_u32(consts::TRACK_ANALYSIS_BYTES_VERSION);
        writer.write_str(self.token().as_str())?;
        writer.write_u32(self.source_sample_rate().get());
        let origin = self.media_origin();
        writer.write_bool(origin.is_some());
        writer.write_u64(origin.map_or(0, |origin| origin.as_secs()));
        writer.write_u32(origin.map_or(0, |origin| origin.subsec_nanos()));
        writer.write_optional_u64(self.extent());
        writer.write_u64(self.revision());
        writer.write_bool(self.is_settled());
        writer.write_coverage(self.coverage());

        let fingerprint = self.fingerprint();
        write_tagged(
            &mut writer,
            fingerprint.waveform(),
            self.waveform(),
            Waveform::write_to,
        )?;
        let beat = self.beat();
        write_tagged(&mut writer, fingerprint.beat(), beat, |beat, out| {
            beat.artifact().write_to(out);
        })?;
        writer.write_bool(beat.is_some_and(|beat| beat.state() == BeatState::Final));
        write_ranges(&mut writer, beat.map_or(&[], BeatSnapshot::unanalysed));
        write_tagged(&mut writer, fingerprint.key(), self.key(), write_key)
    }
}

impl TryFrom<&[u8]> for TrackAnalysis {
    type Error = BlobError;

    /// Read a snapshot as it was written: every artifact under the tag it
    /// carries, whatever analyzers this build has.
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let mut reader = Reader::new(bytes);
        let version = reader.read_u32()?;
        if version != consts::TRACK_ANALYSIS_BYTES_VERSION {
            return Err(BlobError::Version {
                found: version,
                expected: consts::TRACK_ANALYSIS_BYTES_VERSION,
            });
        }
        Self::try_from(RawTrackSnapshot::read(&mut reader)?)
    }
}

struct RawTrackSnapshot<'a> {
    token: String,
    source_sample_rate: NonZeroU32,
    media_origin: Option<Duration>,
    extent: Option<u64>,
    revision: u64,
    settled: bool,
    coverage: RangeSet<u64>,
    waveform: Tagged<'a>,
    beat: Tagged<'a>,
    final_grid: bool,
    unanalysed: Vec<Range<u64>>,
    key: Tagged<'a>,
}

/// An artifact's fingerprint tag and the section holding its bytes.
struct Tagged<'a> {
    tag: String,
    bytes: &'a [u8],
}

impl<'a> RawTrackSnapshot<'a> {
    fn read(reader: &mut Reader<'a>) -> Result<Self, BlobError> {
        let token = reader.read_str()?;
        let (source_sample_rate, media_origin) = read_source_axis(reader)?;
        let extent = reader.read_optional_u64()?;
        let revision = reader.read_u64()?;
        let settled = reader.read_bool()?;
        let coverage = reader.read_coverage()?;
        let waveform = Tagged::read(reader)?;
        let beat = Tagged::read(reader)?;
        let (final_grid, unanalysed) = read_beat_state(reader, &beat)?;
        let key = Tagged::read(reader)?;
        reader.finish()?;
        Ok(Self {
            token,
            source_sample_rate,
            media_origin,
            extent,
            revision,
            settled,
            coverage,
            waveform,
            beat,
            final_grid,
            unanalysed,
            key,
        })
    }
}

impl TryFrom<RawTrackSnapshot<'_>> for TrackAnalysis {
    type Error = BlobError;

    fn try_from(raw: RawTrackSnapshot<'_>) -> Result<Self, Self::Error> {
        let state = if raw.final_grid {
            BeatState::Final
        } else {
            BeatState::Provisional
        };
        let grid = raw
            .beat
            .decode(|bytes| BeatArtifact::try_from(bytes).map_err(|_| BlobError::Corrupt))?;

        Ok(Self::builder()
            .token(raw.token.as_str().into())
            .revision(raw.revision)
            .source_sample_rate(raw.source_sample_rate)
            .maybe_media_origin(raw.media_origin)
            .maybe_extent(raw.extent)
            .settled(raw.settled)
            .coverage(raw.coverage)
            .maybe_waveform(
                raw.waveform
                    .decode(|bytes| Waveform::try_from(bytes).map_err(|_| BlobError::Corrupt))?,
            )
            .maybe_key(raw.key.decode(read_key)?)
            .maybe_beat(grid.map(|grid| BeatSnapshot::new(grid, state, raw.unanalysed)))
            .fingerprint(
                AnalysisFingerprint::builder()
                    .maybe_beat(raw.beat.tag())
                    .maybe_waveform(raw.waveform.tag())
                    .maybe_key(raw.key.tag())
                    .build(),
            )
            .build())
    }
}

impl<'a> Tagged<'a> {
    fn read(reader: &mut Reader<'a>) -> Result<Self, BlobError> {
        let tag = reader.read_str()?;
        Ok(Self {
            tag,
            bytes: reader.read_section()?,
        })
    }

    /// The tag this section was written under; an empty tag names none.
    fn tag(&self) -> Option<&str> {
        (!self.tag.is_empty()).then_some(self.tag.as_str())
    }

    /// The artifact this section holds; an empty section holds none.
    fn decode<T>(
        &self,
        decode: impl FnOnce(&'a [u8]) -> Result<T, BlobError>,
    ) -> Result<Option<T>, BlobError> {
        (!self.bytes.is_empty())
            .then_some(self.bytes)
            .map(decode)
            .transpose()
    }
}

/// The final flag and unanalysed ranges of the beat artifact in `beat`. They
/// belong to that artifact, so stating either without one is corrupt.
fn read_beat_state(
    reader: &mut Reader<'_>,
    beat: &Tagged<'_>,
) -> Result<(bool, Vec<Range<u64>>), BlobError> {
    let final_grid = reader.read_bool()?;
    let unanalysed = read_ranges(reader)?;
    if beat.bytes.is_empty() && (final_grid || !unanalysed.is_empty()) {
        return Err(BlobError::Corrupt);
    }
    Ok((final_grid, unanalysed))
}

fn read_source_axis(reader: &mut Reader<'_>) -> Result<(NonZeroU32, Option<Duration>), BlobError> {
    let source_sample_rate = NonZeroU32::new(reader.read_u32()?).ok_or(BlobError::Corrupt)?;
    let has_origin = reader.read_bool()?;
    let origin_secs = reader.read_u64()?;
    let origin_nanos = reader.read_u32()?;
    if origin_nanos >= 1_000_000_000 || !has_origin && (origin_secs != 0 || origin_nanos != 0) {
        return Err(BlobError::Corrupt);
    }
    Ok((
        source_sample_rate,
        has_origin.then(|| Duration::new(origin_secs, origin_nanos)),
    ))
}

/// An artifact's fingerprint tag, then a section holding the artifact when
/// there is one.
fn write_tagged<T>(
    writer: &mut Writer<'_>,
    tag: Option<&str>,
    artifact: Option<T>,
    write: impl FnOnce(T, &mut Vec<u8>),
) -> Result<(), BlobError> {
    writer.write_str(tag.unwrap_or_default())?;
    writer.write_section(|out| {
        if let Some(artifact) = artifact {
            write(artifact, out);
        }
    })
}

fn write_key(estimate: KeyEstimate, out: &mut Vec<u8>) {
    let mut writer = Writer::new(out);
    let key = estimate.key();
    writer.write_u8(key.tonic.semitone());
    writer.write_bool(key.mode == Mode::Minor);
    writer.write_f32(estimate.strength());
}

fn read_key(bytes: &[u8]) -> Result<KeyEstimate, BlobError> {
    let mut reader = Reader::new(bytes);
    let tonic = PitchClass::try_from(reader.read_u8()?).map_err(|_| BlobError::Corrupt)?;
    let mode = if reader.read_bool()? {
        Mode::Minor
    } else {
        Mode::Major
    };
    let strength = reader.read_f32()?;
    reader.finish()?;
    KeyEstimate::new(MusicalKey { mode, tonic }, strength).ok_or(BlobError::Corrupt)
}

fn write_ranges(writer: &mut Writer<'_>, ranges: &[Range<u64>]) {
    writer.write_len(ranges.len());
    for range in ranges {
        writer.write_u64(range.start);
        writer.write_u64(range.frames());
    }
}

fn read_ranges(reader: &mut Reader<'_>) -> Result<Vec<Range<u64>>, BlobError> {
    let count = reader.read_count(16)?;
    let mut ranges: Vec<Range<u64>> = Vec::with_capacity(count.min(MAX_PREALLOC));
    for _ in 0..count {
        let start = reader.read_u64()?;
        let frames = reader.read_u64()?;
        ranges.push(start..start.saturating_add(frames));
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};

    use kithara_test_utils::kithara;
    use rangemap::RangeSet;

    use super::*;
    use crate::{Bucket, artifact::FitRegion};

    fn fingerprint(wave: &str, beat: &str) -> AnalysisFingerprint {
        AnalysisFingerprint::builder()
            .beat(beat)
            .waveform(wave)
            .build()
    }

    fn active() -> AnalysisFingerprint {
        fingerprint("wave:fold1:v3", consts::BEAT_TAG)
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(44_100).expect("fixture rate is non-zero")
    }

    fn wave() -> Waveform {
        Waveform::try_new(vec![Bucket::new([0.5; 4], [0.5; 4])], NonZeroU64::MIN)
            .expect("hand-built buckets are valid")
    }

    fn grid() -> BeatArtifact {
        BeatArtifact::with_regions(
            128.0,
            vec![(0, Some(0.9)), (10_000, Some(0.75)), (20_000, None)],
            vec![(0, Some(0.9)), (40_000, None)],
            vec![FitRegion::new(0, 40_000, 1.01)],
        )
    }

    /// Eight beats half a second apart, so the artifact states a grid.
    fn steady_grid() -> BeatArtifact {
        BeatArtifact::new(
            consts::BPM,
            (0..8)
                .map(|beat| (beat * consts::PERIOD_44_1, Some(0.8)))
                .collect(),
            vec![(0, Some(0.7)), (4 * consts::PERIOD_44_1, Some(0.6))],
        )
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

    fn analysis(
        beat: Option<BeatArtifact>,
        waveform: Option<Waveform>,
        extent: u64,
    ) -> TrackAnalysis {
        let mut coverage = RangeSet::new();
        if extent > 0 {
            coverage.insert(0..extent);
        }
        TrackAnalysis::builder()
            .media_origin(Duration::ZERO)
            .token(consts::TRACK_TOKEN.into())
            .revision(7)
            .source_sample_rate(rate())
            .extent(extent)
            .settled(true)
            .coverage(coverage)
            .fingerprint(active())
            .maybe_waveform(waveform)
            .maybe_beat(beat.map(|grid| {
                BeatSnapshot::new(
                    grid,
                    BeatState::Provisional,
                    vec![Range {
                        start: 100,
                        end: 150,
                    }],
                )
            }))
            .build()
    }

    fn encode(analysis: &TrackAnalysis) -> Vec<u8> {
        let mut bytes = Vec::new();
        analysis.write_to(&mut bytes).expect("encodes");
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<TrackAnalysis, BlobError> {
        TrackAnalysis::try_from(bytes)
    }

    #[kithara::test]
    fn frozen_v5_v7_and_v8_fixtures_are_refused() {
        for (bytes, version) in [
            (consts::V5_FIXTURE, 0x4b41_0005),
            (consts::V7_FIXTURE, 0x4b41_0007),
            (consts::V8_FIXTURE, 0x4b41_0008),
        ] {
            assert!(matches!(
                decode(bytes),
                Err(BlobError::Version {
                    found,
                    expected: consts::TRACK_ANALYSIS_BYTES_VERSION,
                }) if found == version
            ));
        }
    }

    #[kithara::test]
    fn frozen_v9_fixture_decodes_and_reencodes_identically() {
        let decoded = decode(consts::V9_FIXTURE).expect("v9 decodes");

        assert_eq!(decoded.token().as_str(), "golden-v9");
        assert_eq!(decoded.source_sample_rate().get(), 44_100);
        assert_eq!(
            decoded.media_origin(),
            Some(Duration::from_nanos(25_056_689))
        );
        assert_eq!(decoded.extent(), Some(1_234));
        assert_eq!(decoded.revision(), 9);
        assert!(!decoded.is_settled());
        assert_eq!(
            decoded.coverage().iter().collect::<Vec<_>>(),
            [&(0..100), &(200..250)]
        );
        assert_eq!(
            decoded.fingerprint(),
            &AnalysisFingerprint::builder()
                .beat("beat:v1")
                .waveform("wave:v1")
                .key("key:v1")
                .build()
        );
        assert!(decoded.waveform().is_none());
        assert!(decoded.beat().is_none());
        assert_eq!(decoded.key(), Some(a_minor()));

        let mut encoded = Vec::new();
        decoded.write_to(&mut encoded).expect("v9 re-encodes");
        assert_eq!(encoded.as_slice(), consts::V9_FIXTURE);
    }

    /// A reader keeps no analyzer of its own: a density, a beat model and a
    /// key estimator it never ran are read as the writer tagged them.
    #[kithara::test]
    fn a_document_reads_every_artifact_under_the_tag_it_carries() {
        let span = NonZeroU64::new(10 * 441).expect("ten windows span frames");
        let waveform = Waveform::try_new(
            vec![
                Bucket::new([0.25, 0.5, 0.75, 0.5], [0.25, 0.5, 0.75, 0.5]),
                Bucket::new([0.5, 0.25, 0.125, 0.25], [0.5, 0.25, 0.125, 0.25]),
            ],
            span,
        )
        .expect("hand-built buckets are valid");
        let tags = AnalysisFingerprint::builder()
            .waveform("wave:fold10:v2")
            .beat("beat:nn:a-model-no-build-embeds:v1")
            .key("key:another-estimator:v1")
            .build();
        let mut coverage = RangeSet::new();
        coverage.insert(0..8 * consts::PERIOD_44_1);
        let want = TrackAnalysis::builder()
            .media_origin(Duration::new(0, 25_056_689))
            .token(consts::TRACK_TOKEN.into())
            .revision(4)
            .source_sample_rate(rate())
            .extent(8 * consts::PERIOD_44_1)
            .settled(true)
            .coverage(coverage)
            .fingerprint(tags.clone())
            .waveform(waveform.clone())
            .beat(BeatSnapshot::new(
                steady_grid(),
                BeatState::Final,
                Vec::new(),
            ))
            .key(a_minor())
            .build();

        let got = decode(&encode(&want)).expect("a document decodes");

        assert_eq!(got.fingerprint(), &tags);
        assert_eq!(got.source_sample_rate(), want.source_sample_rate());
        assert_eq!(got.media_origin(), want.media_origin());
        assert_eq!(got.extent(), want.extent());
        assert_eq!(got.coverage(), want.coverage());
        assert!(got.is_complete());
        let got_waveform = got.waveform().expect("the waveform is read");
        assert_eq!(got_waveform.span_frames(), span);
        assert_eq!(got_waveform, &waveform);
        assert_eq!(got.beat().map(BeatSnapshot::artifact), Some(&steady_grid()));
        assert_eq!(got.beat().map(BeatSnapshot::state), Some(BeatState::Final));
        assert!(want.grid().is_some(), "the fixture states a grid");
        assert_eq!(got.grid(), want.grid());
        assert_eq!(got.key(), Some(a_minor()));
    }

    #[kithara::test]
    fn a_beat_and_key_document_reads_without_a_waveform() {
        let want = TrackAnalysis::builder()
            .media_origin(Duration::ZERO)
            .token(consts::TRACK_TOKEN.into())
            .revision(2)
            .source_sample_rate(rate())
            .extent(8 * consts::PERIOD_44_1)
            .settled(true)
            .fingerprint(
                AnalysisFingerprint::builder()
                    .beat(consts::BEAT_TAG)
                    .key("key:test:v1")
                    .build(),
            )
            .beat(BeatSnapshot::new(
                steady_grid(),
                BeatState::Final,
                Vec::new(),
            ))
            .key(a_minor())
            .build();

        let got = decode(&encode(&want)).expect("a document decodes");

        assert!(got.waveform().is_none());
        assert_eq!(got.fingerprint().waveform(), None);
        assert!(got.grid().is_some(), "the beat artifact states a grid");
        assert_eq!(got.key(), Some(a_minor()));
    }

    #[kithara::test]
    fn a_settled_document_with_a_coverage_gap_reads_as_incomplete() {
        let mut coverage = RangeSet::new();
        coverage.insert(0..400);
        coverage.insert(600..1_000);
        let want = TrackAnalysis::builder()
            .token(consts::TRACK_TOKEN.into())
            .revision(5)
            .source_sample_rate(rate())
            .extent(1_000)
            .settled(true)
            .coverage(coverage)
            .fingerprint(active())
            .waveform(wave())
            .build();

        let got = decode(&encode(&want)).expect("a document decodes");

        assert!(got.is_settled());
        assert!(!got.is_complete());
        assert_eq!(
            got.missing(),
            vec![Range {
                start: 400,
                end: 600
            }]
        );
        assert_eq!(got.missing(), want.missing());
    }

    #[kithara::test]
    fn a_foreign_version_and_a_truncated_document_are_refused() {
        let bytes = encode(&analysis(Some(grid()), Some(wave()), 1));
        let mut foreign = bytes.clone();
        foreign[..4].copy_from_slice(&(consts::TRACK_ANALYSIS_BYTES_VERSION + 1).to_le_bytes());
        assert!(matches!(
            decode(&foreign),
            Err(BlobError::Version { found, expected })
                if found == consts::TRACK_ANALYSIS_BYTES_VERSION + 1
                    && expected == consts::TRACK_ANALYSIS_BYTES_VERSION
        ));
        for cut in [1, 4, bytes.len() / 2] {
            assert!(
                matches!(decode(&bytes[..bytes.len() - cut]), Err(BlobError::Corrupt)),
                "a document cut {cut} bytes short"
            );
        }
    }

    #[kithara::test]
    fn every_snapshot_field_round_trips() {
        let mut coverage = RangeSet::new();
        coverage.insert(0..400);
        coverage.insert(600..1000);
        let full = TrackAnalysis::builder()
            .media_origin(Duration::ZERO)
            .token(consts::TRACK_TOKEN.into())
            .revision(11)
            .source_sample_rate(rate())
            .extent(1_000)
            .coverage(coverage)
            .fingerprint(active())
            .waveform(wave())
            .beat(BeatSnapshot::new(
                grid(),
                BeatState::Provisional,
                vec![Range {
                    start: 400,
                    end: 600,
                }],
            ))
            .build();
        let mut empty_coverage = RangeSet::new();
        empty_coverage.insert(0..64);
        let empty = TrackAnalysis::builder()
            .media_origin(Duration::ZERO)
            .token("empty-fingerprint".into())
            .revision(1)
            .source_sample_rate(rate())
            .extent(64)
            .coverage(empty_coverage)
            .settled(true)
            .fingerprint(AnalysisFingerprint::default())
            .build();
        for want in [
            full,
            analysis(None, Some(wave()), 0),
            analysis(Some(grid()), None, 0),
            empty,
        ] {
            let got = decode(&encode(&want)).expect("decodes");

            assert_eq!(got.token(), want.token());
            assert_eq!(got.source_sample_rate(), want.source_sample_rate());
            assert_eq!(got.media_origin(), want.media_origin());
            assert_eq!(got.extent(), want.extent());
            assert_eq!(got.source_frames(), want.source_frames());
            assert_eq!(got.coverage(), want.coverage());
            assert_eq!(got.revision(), want.revision());
            assert_eq!(got.is_settled(), want.is_settled());
            assert_eq!(got.fingerprint(), want.fingerprint());
            assert_eq!(got.waveform(), want.waveform());
            assert_eq!(
                got.beat().map(BeatSnapshot::artifact),
                want.beat().map(BeatSnapshot::artifact)
            );
            assert_eq!(
                got.beat().map(BeatSnapshot::state),
                want.beat().map(BeatSnapshot::state)
            );
            assert_eq!(
                got.beat().map(BeatSnapshot::unanalysed),
                want.beat().map(BeatSnapshot::unanalysed)
            );
        }
    }

    #[kithara::test]
    fn rejects_non_boolean_flags_and_corrupt_sections() {
        let bytes = encode(&analysis(Some(grid()), Some(wave()), 1));
        let origin_flag = consts::TRACK_ANALYSIS_BYTES_VERSION.to_le_bytes().len()
            + 4
            + consts::TRACK_TOKEN.len()
            + 4;
        let (waveform_offset, grid_offset) = {
            let mut reader = Reader::new(&bytes);
            reader.read_u32().expect("version");
            reader.read_str().expect("token");
            read_source_axis(&mut reader).expect("source axis");
            reader.read_optional_u64().expect("extent");
            reader.read_u64().expect("revision");
            reader.read_bool().expect("settled");
            reader.read_coverage().expect("coverage");
            reader.read_str().expect("waveform fingerprint");
            let waveform_offset = bytes.len() - reader.remaining() + 8;
            reader.read_section().expect("waveform");
            reader.read_str().expect("beat fingerprint");
            let grid_offset = bytes.len() - reader.remaining() + 8;
            (waveform_offset, grid_offset)
        };
        for (name, offset) in [
            ("non-boolean origin", origin_flag),
            ("corrupt waveform", waveform_offset),
            ("corrupt grid", grid_offset),
        ] {
            let mut corrupt = bytes.clone();
            corrupt[offset] = 0xff;
            assert!(
                matches!(decode(&corrupt), Err(BlobError::Corrupt)),
                "{name}"
            );
        }
    }

    /// A final flag or unanalysed ranges belong to a beat artifact, so a
    /// snapshot stating either without one is corrupt.
    #[kithara::test]
    fn beat_state_without_a_beat_artifact_is_corrupt() {
        let empty = TrackAnalysis::builder()
            .token("empty-fingerprint".into())
            .revision(1)
            .source_sample_rate(rate())
            .build();
        let bytes = encode(&empty);
        let tail = 1 + 8 + 4 + 8;
        for final_grid in [false, true] {
            let mut inconsistent = bytes[..bytes.len() - tail].to_vec();
            let mut writer = Writer::new(&mut inconsistent);
            writer.write_bool(final_grid);
            write_ranges(
                &mut writer,
                if final_grid {
                    &[]
                } else {
                    &[Range { start: 0, end: 1 }]
                },
            );
            writer.write_str("").expect("an empty tag fits");
            writer.write_len(0);
            assert!(matches!(decode(&inconsistent), Err(BlobError::Corrupt)));
        }
    }

    #[kithara::test]
    fn media_origin_round_trips_without_losing_subsecond_precision() {
        for origin in [
            None,
            Some(Duration::ZERO),
            Some(Duration::new(3, 25_056_689)),
        ] {
            let want = TrackAnalysis::builder()
                .token(consts::TRACK_TOKEN.into())
                .revision(1)
                .source_sample_rate(rate())
                .maybe_media_origin(origin)
                .beat(BeatSnapshot::new(grid(), BeatState::Final, Vec::new()))
                .fingerprint(active())
                .build();
            let back = decode(&encode(&want)).expect("the blob decodes");
            assert_eq!(back.media_origin(), origin);
            assert_eq!(
                back.beat().expect("raw markers survive").artifact(),
                &grid()
            );
        }
    }

    #[kithara::test]
    fn media_origin_rejects_noncanonical_absence_and_nanoseconds() {
        let want = analysis(Some(grid()), None, 0);
        let origin_flag = 4 + 4 + consts::TRACK_TOKEN.len() + 4;
        let original = encode(&want);
        for (present, seconds, nanos) in
            [(false, 1u64, 0u32), (false, 0, 1), (true, 0, 1_000_000_000)]
        {
            let mut bytes = original.clone();
            bytes[origin_flag] = u8::from(present);
            bytes[origin_flag + 1..origin_flag + 9].copy_from_slice(&seconds.to_le_bytes());
            bytes[origin_flag + 9..origin_flag + 13].copy_from_slice(&nanos.to_le_bytes());
            assert!(matches!(decode(&bytes), Err(BlobError::Corrupt)));
        }
    }
}
