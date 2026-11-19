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

impl TryFrom<(&[u8], &AnalysisFingerprint)> for TrackAnalysis {
    type Error = BlobError;

    fn try_from((bytes, active): (&[u8], &AnalysisFingerprint)) -> Result<Self, Self::Error> {
        let mut reader = Reader::new(bytes);
        let version = reader.read_u32()?;
        if version != consts::TRACK_ANALYSIS_BYTES_VERSION {
            return Err(BlobError::Version {
                found: version,
                expected: consts::TRACK_ANALYSIS_BYTES_VERSION,
            });
        }
        RawTrackSnapshot::read(&mut reader)?.into_analysis(active)
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
        let final_grid = reader.read_bool()?;
        let unanalysed = read_ranges(reader)?;
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

    /// Whether the active configuration still produces something stored
    /// here; a snapshot with nothing in it is served under an empty
    /// configuration.
    fn check_served(&self, active: &AnalysisFingerprint) -> Result<(), BlobError> {
        let served = self.waveform.matches(active.waveform())
            || self.beat.matches(active.beat())
            || self.key.matches(active.key());
        if served || active == &AnalysisFingerprint::default() && self.is_blank() {
            return Ok(());
        }
        Err(BlobError::Fingerprint)
    }

    fn is_blank(&self) -> bool {
        self.waveform.is_blank()
            && self.beat.is_blank()
            && self.key.is_blank()
            && !self.final_grid
            && self.unanalysed.is_empty()
    }

    fn into_analysis(self, active: &AnalysisFingerprint) -> Result<TrackAnalysis, BlobError> {
        self.check_served(active)?;
        let waveform = self.waveform.served(active.waveform());
        let beat = self.beat.served(active.beat());
        let key = self.key.served(active.key());
        let state = if self.final_grid {
            BeatState::Final
        } else {
            BeatState::Provisional
        };
        let grid = Tagged::decode(beat.as_ref(), |bytes| {
            BeatArtifact::try_from(bytes).map_err(|_| BlobError::Corrupt)
        })?;

        Ok(TrackAnalysis::builder()
            .token(self.token.as_str().into())
            .revision(self.revision)
            .source_sample_rate(self.source_sample_rate)
            .maybe_media_origin(self.media_origin)
            .maybe_extent(self.extent)
            .settled(self.settled)
            .coverage(self.coverage)
            .maybe_waveform(Tagged::decode(waveform.as_ref(), |bytes| {
                Waveform::try_from(bytes).map_err(|_| BlobError::Corrupt)
            })?)
            .maybe_key(Tagged::decode(key.as_ref(), read_key)?)
            .maybe_beat(grid.map(|grid| BeatSnapshot::new(grid, state, self.unanalysed)))
            .fingerprint(
                AnalysisFingerprint::builder()
                    .maybe_beat(beat.map(|tagged| tagged.tag))
                    .maybe_waveform(waveform.map(|tagged| tagged.tag))
                    .maybe_key(key.map(|tagged| tagged.tag))
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

    /// Whether `active` is the configuration that wrote this section.
    fn matches(&self, active: Option<&str>) -> bool {
        active.is_some_and(|tag| self.tag == tag)
    }

    fn is_blank(&self) -> bool {
        self.tag.is_empty() && self.bytes.is_empty()
    }

    /// This section when `active` wrote it, and nothing otherwise.
    fn served(self, active: Option<&str>) -> Option<Self> {
        self.matches(active).then_some(self)
    }

    /// The artifact a served section holds; a served empty section holds
    /// none.
    fn decode<T>(
        served: Option<&Self>,
        decode: impl FnOnce(&'a [u8]) -> Result<T, BlobError>,
    ) -> Result<Option<T>, BlobError> {
        served
            .map(|tagged| tagged.bytes)
            .filter(|bytes| !bytes.is_empty())
            .map(decode)
            .transpose()
    }
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
        fingerprint("wave:fold1:v2", consts::BEAT_TAG)
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(44_100).expect("fixture rate is non-zero")
    }

    fn wave() -> Waveform {
        Waveform::new(vec![Bucket::new(0.5, 0.5, 0.5)], NonZeroU64::MIN)
            .expect("hand-built blob is valid")
    }

    fn grid() -> BeatArtifact {
        BeatArtifact::with_regions(
            128.0,
            vec![(0, Some(0.9)), (10_000, Some(0.75)), (20_000, None)],
            vec![(0, Some(0.9)), (40_000, None)],
            vec![FitRegion::new(0, 40_000, 1.01)],
        )
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

    #[kithara::test]
    fn frozen_v5_v7_and_v8_fixtures_are_cache_misses() {
        let active = fingerprint("wave:v1", "beat:v1");
        for (bytes, version) in [
            (consts::V5_FIXTURE, 0x4b41_0005),
            (consts::V7_FIXTURE, 0x4b41_0007),
            (consts::V8_FIXTURE, 0x4b41_0008),
        ] {
            assert!(matches!(
                TrackAnalysis::try_from((bytes, &active)),
                Err(BlobError::Version {
                    found,
                    expected: consts::TRACK_ANALYSIS_BYTES_VERSION,
                }) if found == version
            ));
        }
    }

    #[kithara::test]
    fn frozen_v9_fixture_decodes_and_reencodes_identically() {
        let active = AnalysisFingerprint::builder()
            .beat("beat:v1")
            .waveform("wave:v1")
            .key("key:v1")
            .build();
        let decoded = TrackAnalysis::try_from((consts::V9_FIXTURE, &active)).expect("v9 decodes");

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
        assert_eq!(decoded.fingerprint(), &active);
        assert!(decoded.waveform().is_none());
        assert!(decoded.beat().is_none());
        assert_eq!(
            decoded.key(),
            KeyEstimate::new(
                MusicalKey {
                    tonic: PitchClass::A,
                    mode: Mode::Minor,
                },
                0.875,
            )
        );

        let mut encoded = Vec::new();
        decoded.write_to(&mut encoded).expect("v9 re-encodes");
        assert_eq!(encoded.as_slice(), consts::V9_FIXTURE);
    }

    #[kithara::test]
    fn stale_fingerprint_is_a_miss() {
        assert!(matches!(
            TrackAnalysis::try_from((
                &encode(&analysis(Some(grid()), Some(wave()), 1))[..],
                &fingerprint("other-wave", "other-beat"),
            )),
            Err(BlobError::Fingerprint)
        ));

        let empty = TrackAnalysis::builder()
            .token("empty-fingerprint".into())
            .revision(1)
            .source_sample_rate(rate())
            .build();
        let bytes = encode(&empty);
        // The beat flag and ranges, then an empty key tag and key section.
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
            assert!(matches!(
                TrackAnalysis::try_from(
                    (inconsistent.as_slice(), &AnalysisFingerprint::default(),)
                ),
                Err(BlobError::Fingerprint)
            ));
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
            let bytes = encode(&want);
            let got = TrackAnalysis::try_from((&bytes[..], want.fingerprint())).expect("decodes");

            assert_eq!(got.token(), want.token());
            assert_eq!(got.source_sample_rate(), want.source_sample_rate());
            assert_eq!(got.media_origin(), want.media_origin());
            assert_eq!(got.extent(), want.extent());
            assert_eq!(got.source_frames(), want.source_frames());
            assert_eq!(got.coverage(), want.coverage());
            assert_eq!(got.revision(), want.revision());
            assert_eq!(got.is_settled(), want.is_settled());
            assert_eq!(got.fingerprint(), want.fingerprint());
            assert_eq!(
                got.waveform().map(Waveform::buckets),
                want.waveform().map(Waveform::buckets)
            );
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
    fn a_key_round_trips_under_its_own_tag() {
        let estimate = KeyEstimate::new(
            MusicalKey {
                tonic: PitchClass::A,
                mode: Mode::Minor,
            },
            0.875,
        )
        .expect("a fixture strength is in range");
        let tagged = |key: &str| {
            AnalysisFingerprint::builder()
                .beat(consts::BEAT_TAG)
                .waveform("wave:fold1:v2")
                .key(key)
                .build()
        };
        let want = TrackAnalysis::builder()
            .token(consts::TRACK_TOKEN.into())
            .revision(3)
            .source_sample_rate(rate())
            .extent(1_000)
            .settled(true)
            .fingerprint(tagged("key:test:v1"))
            .beat(BeatSnapshot::new(grid(), BeatState::Final, Vec::new()))
            .key(estimate)
            .build();
        let bytes = encode(&want);

        let got = TrackAnalysis::try_from((&bytes[..], want.fingerprint())).expect("decodes");
        assert_eq!(got.key(), Some(estimate));
        assert_eq!(got.fingerprint(), want.fingerprint());

        let other = TrackAnalysis::try_from((&bytes[..], &tagged("key:other:v1")))
            .expect("the beat and waveform tags still match");
        assert_eq!(other.key(), None, "another key configuration is a key miss");
        assert_eq!(other.fingerprint().key(), None);
        assert_eq!(other.fingerprint().beat(), Some(consts::BEAT_TAG));
        assert_eq!(
            other.beat().map(BeatSnapshot::artifact),
            Some(&grid()),
            "the beat result stays usable"
        );
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
                matches!(
                    TrackAnalysis::try_from((&corrupt[..], &active())),
                    Err(BlobError::Corrupt)
                ),
                "{name}"
            );
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
            let bytes = encode(&want);
            let back = TrackAnalysis::try_from((&bytes[..], &active())).expect("the blob decodes");
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
            assert!(matches!(
                TrackAnalysis::try_from((&bytes[..], &active())),
                Err(BlobError::Corrupt)
            ));
        }
    }
}
