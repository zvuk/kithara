use std::num::{NonZeroU32, NonZeroU64};

use kithara_platform::time::Duration;
#[cfg(all(feature = "beat-nn", feature = "analysis-waveform"))]
use kithara_resampler::rubato::RubatoBackend;
use kithara_test_utils::kithara;
use rangemap::RangeSet;

use super::{AnalysisFile, AnalysisFileError, AnalysisFileSpec, AnalysisFileUpdate};
#[cfg(all(feature = "beat-nn", feature = "analysis-waveform"))]
use crate::test_pools::pools;
use crate::{
    AnalysisFingerprint, AnalysisProgress, TrackAnalysis, blob::Writer, consts,
    progress::AnalysisResume,
};

fn rate(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
}

fn chunk_frames(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN)
}

fn fingerprint() -> AnalysisFingerprint {
    AnalysisFingerprint::new(Some("beat:test:v1"), Some("wave:test:v1"))
}

fn spec() -> AnalysisFileSpec {
    AnalysisFileSpec::new(
        rate(48_000),
        consts::ARCHIVE_EXTENT,
        chunk_frames(consts::ARCHIVE_CHUNK_FRAMES),
        fingerprint(),
    )
    .expect("fixture spec is valid")
}

fn analysis(revision: u64, ranges: &[(u64, u64)], settled: bool) -> TrackAnalysis {
    analysis_with(
        revision,
        rate(48_000),
        Some(consts::ARCHIVE_EXTENT),
        fingerprint(),
        ranges,
        settled,
    )
}

fn analysis_with(
    revision: u64,
    source_sample_rate: NonZeroU32,
    extent: Option<u64>,
    fingerprint: AnalysisFingerprint,
    ranges: &[(u64, u64)],
    settled: bool,
) -> TrackAnalysis {
    let mut coverage = RangeSet::new();
    for &(start, frames) in ranges {
        coverage.insert(start..start + frames);
    }
    TrackAnalysis::builder()
        .token("archive:test".into())
        .revision(revision)
        .source_sample_rate(source_sample_rate)
        .maybe_extent(extent)
        .coverage(coverage)
        .fingerprint(fingerprint)
        .settled(settled)
        .maybe_waveform(None)
        .maybe_beat(None)
        .build()
}

fn progress(analysis: TrackAnalysis, chunk_frames: NonZeroU64) -> AnalysisProgress {
    let resume = if analysis.is_settled() {
        None
    } else {
        let section = || {
            let mut out = Vec::new();
            let mut writer = Writer::new(&mut out);
            writer.write_len(0);
            writer.write_len(0);
            writer.write_u64(0);
            out
        };
        let beat = || {
            let mut out = Vec::new();
            let mut writer = Writer::new(&mut out);
            for _ in 0..4 {
                writer.write_len(0);
            }
            out
        };
        let waveform = analysis.fingerprint().waveform().map(|_| section());
        let beat = analysis.fingerprint().beat().map(|_| beat());
        Some(AnalysisResume::capture(
            chunk_frames,
            waveform.as_deref(),
            beat.as_deref(),
        ))
    };
    AnalysisProgress::new(analysis, resume)
}

fn default_progress(analysis: TrackAnalysis) -> AnalysisProgress {
    progress(analysis, chunk_frames(consts::ARCHIVE_CHUNK_FRAMES))
}

fn apply(bytes: &mut Vec<u8>, update: &AnalysisFileUpdate) {
    if let Some(initial) = update.initial_bytes() {
        assert!(bytes.is_empty(), "only create supplies initial bytes");
        bytes.extend_from_slice(initial);
    }

    let payload = update.payload();
    let payload_start = usize::try_from(payload.offset()).expect("fixture offset fits usize");
    let payload_end = payload_start
        .checked_add(payload.bytes().len())
        .expect("fixture payload end fits usize");
    if payload_end > bytes.len() {
        bytes.resize(payload_end, 0);
    }
    bytes[payload_start..payload_end].copy_from_slice(payload.bytes());

    for patch in update.patches() {
        let start = usize::try_from(patch.offset()).expect("fixture offset fits usize");
        let end = start
            .checked_add(patch.bytes().len())
            .expect("fixture patch end fits usize");
        bytes
            .get_mut(start..end)
            .expect("fixture patch stays inside committed length")
            .copy_from_slice(patch.bytes());
    }

    let final_len = usize::try_from(update.final_len()).expect("fixture length fits usize");
    bytes.truncate(final_len);
    assert_eq!(bytes.len(), final_len);
}

fn create_bytes(analysis: TrackAnalysis) -> Vec<u8> {
    let progress = default_progress(analysis);
    let update = AnalysisFile::create(&spec(), &progress).expect("create succeeds");
    let mut bytes = Vec::new();
    apply(&mut bytes, &update);
    bytes
}

#[cfg(all(feature = "beat-nn", feature = "analysis-waveform"))]
#[kithara::test(native, flash(false))]
fn effective_application_fingerprint_fits_the_fixed_header() {
    let fingerprint = crate::AnalyzerBuilder::<RubatoBackend, _>::new(pools())
        .with_beat()
        .with_waveform(2_000)
        .fingerprint();
    assert!(
        fingerprint.beat().is_some_and(|value| value.len() > 256),
        "fixture must exceed the former header limit"
    );
    let snapshot = analysis_with(
        1,
        rate(48_000),
        Some(consts::ARCHIVE_EXTENT),
        fingerprint.clone(),
        &[(0, consts::ARCHIVE_EXTENT)],
        true,
    );
    let spec = AnalysisFileSpec::new(
        rate(48_000),
        consts::ARCHIVE_EXTENT,
        chunk_frames(consts::ARCHIVE_CHUNK_FRAMES),
        fingerprint.clone(),
    )
    .expect("effective application fingerprint fits the header");
    let update = AnalysisFile::create(&spec, &default_progress(snapshot))
        .expect("effective application checkpoint is encoded");
    let mut bytes = Vec::new();
    apply(&mut bytes, &update);

    AnalysisFile::parse(&bytes, &fingerprint).expect("effective application checkpoint restores");
}

#[kithara::test]
fn partial_unsettled_snapshot_round_trips() {
    let bytes = create_bytes(analysis(1, &[(0, consts::ARCHIVE_CHUNK_FRAMES)], false));
    let file = AnalysisFile::parse(&bytes, &fingerprint()).expect("file restores");

    assert_eq!(file.spec().source_sample_rate(), rate(48_000));
    assert_eq!(file.spec().extent(), consts::ARCHIVE_EXTENT);
    assert_eq!(
        file.spec().chunk_frames(),
        chunk_frames(consts::ARCHIVE_CHUNK_FRAMES)
    );
    assert_eq!(file.latest().analysis().revision(), 1);
    assert!(!file.latest().analysis().is_settled());
    assert!(file.latest().is_resumable());
    assert_eq!(
        file.latest()
            .analysis()
            .coverage()
            .iter()
            .collect::<Vec<_>>(),
        [&(0..consts::ARCHIVE_CHUNK_FRAMES)]
    );
    assert_eq!(
        &bytes[consts::HEADER_LEN..consts::HEADER_LEN + 4],
        &[1, 0, 0, 0]
    );
}

#[kithara::test]
fn unknown_extent_cannot_size_the_fixed_index() {
    let snapshot = analysis_with(1, rate(48_000), None, fingerprint(), &[(0, 16)], false);

    assert!(matches!(
        AnalysisFileSpec::for_analysis(&snapshot, chunk_frames(consts::ARCHIVE_CHUNK_FRAMES)),
        Err(AnalysisFileError::UnknownExtent)
    ));
}

#[kithara::test]
fn restored_layout_exposes_exact_configured_chunk_duration() {
    let extent = 3_072_000;
    let persisted_spec =
        AnalysisFileSpec::new(rate(48_000), extent, chunk_frames(768_000), fingerprint())
            .expect("fixture spec is valid");
    let snapshot = analysis_with(
        1,
        rate(48_000),
        Some(extent),
        fingerprint(),
        &[(0, 768_000)],
        false,
    );
    let progress = progress(snapshot, chunk_frames(768_000));
    let update = AnalysisFile::create(&persisted_spec, &progress).expect("create succeeds");
    let mut bytes = Vec::new();
    apply(&mut bytes, &update);

    let restored = AnalysisFile::parse(&bytes, &fingerprint()).expect("file restores");
    assert!(
        restored
            .spec()
            .matches_chunk_duration(Duration::from_secs(16))
    );
    assert!(
        !restored
            .spec()
            .matches_chunk_duration(Duration::from_secs(8))
    );
}

#[kithara::test]
fn updates_replace_latest_snapshot_and_preserve_completed_index_entries() {
    let first = analysis(1, &[(0, 16), (32, 16)], false);
    let mut bytes = create_bytes(first);
    let first_len = bytes.len();
    let payload_offset = consts::HEADER_LEN + 4 * consts::INDEX_ENTRY_LEN;
    let first_entry =
        bytes[consts::HEADER_LEN..consts::HEADER_LEN + consts::INDEX_ENTRY_LEN].to_vec();

    let file = AnalysisFile::parse(&bytes, &fingerprint()).expect("first generation restores");
    let second_progress = default_progress(analysis(2, &[(0, 48)], false));
    let second = file
        .update(&second_progress)
        .expect("second generation builds");
    assert!(second.initial_bytes().is_none());
    assert_eq!(
        second.payload().offset(),
        u64::try_from(payload_offset).expect("fixture offset fits u64")
    );
    assert_eq!(
        second.final_len(),
        second.payload().offset()
            + u64::try_from(second.payload().bytes().len()).expect("fixture length fits u64")
    );
    assert!(
        second.final_len() < u64::try_from(first_len).expect("fixture length fits u64"),
        "replacing a smaller latest snapshot truncates stale payload bytes"
    );
    let (header_patch, index_patches) = second
        .patches()
        .split_last()
        .expect("every generation ends with a header patch");
    assert_eq!(header_patch.offset(), 0);
    let header_len = u64::try_from(consts::HEADER_LEN).expect("header length fits u64");
    assert!(
        index_patches
            .iter()
            .all(|patch| patch.offset() >= header_len)
    );
    apply(&mut bytes, &second);

    assert_eq!(
        &bytes[consts::HEADER_LEN..consts::HEADER_LEN + consts::INDEX_ENTRY_LEN],
        first_entry
    );

    let file = AnalysisFile::parse(&bytes, &fingerprint()).expect("second generation restores");
    let first_two_entries =
        bytes[consts::HEADER_LEN..consts::HEADER_LEN + 2 * consts::INDEX_ENTRY_LEN].to_vec();
    let third_progress = default_progress(analysis(3, &[(0, consts::ARCHIVE_EXTENT)], true));
    let third = file
        .update(&third_progress)
        .expect("third generation builds");
    assert_eq!(third.payload().offset(), second.payload().offset());
    apply(&mut bytes, &third);

    assert_eq!(
        &bytes[consts::HEADER_LEN..consts::HEADER_LEN + 2 * consts::INDEX_ENTRY_LEN],
        first_two_entries
    );
    let restored = AnalysisFile::parse(&bytes, &fingerprint()).expect("latest generation restores");
    assert_eq!(restored.latest().analysis().revision(), 3);
    assert!(restored.latest().analysis().is_settled());
    assert!(!restored.latest().is_resumable());
    assert_eq!(
        restored
            .latest()
            .analysis()
            .coverage()
            .iter()
            .collect::<Vec<_>>(),
        [&(0..consts::ARCHIVE_EXTENT)]
    );
    assert_eq!(
        &bytes[consts::HEADER_LEN..consts::HEADER_LEN + 4],
        &[1, 1, 1, 1]
    );
}

#[kithara::test]
fn update_rejects_stale_revision() {
    let bytes = create_bytes(analysis(7, &[(0, 16)], false));
    let file = AnalysisFile::parse(&bytes, &fingerprint()).expect("file restores");

    for incoming in [6, 7] {
        let incoming_progress = default_progress(analysis(incoming, &[(0, 16)], false));
        assert!(matches!(
            file.update(&incoming_progress),
            Err(AnalysisFileError::StaleRevision {
                stored: 7,
                incoming: value,
            }) if value == incoming
        ));
    }
}

#[kithara::test]
fn parse_rejects_fingerprint_axis_extent_and_chunk_drift() {
    const SOURCE_SAMPLE_RATE_FIELD: usize = 16;
    const EXTENT_FIELD: usize = 24;
    const CHUNK_FRAMES_FIELD: usize = 32;

    let bytes = create_bytes(analysis(1, &[(0, 16)], false));
    let other_fingerprint = AnalysisFingerprint::new(Some("beat:other"), Some("wave:other"));
    assert!(matches!(
        AnalysisFile::parse(&bytes, &other_fingerprint),
        Err(AnalysisFileError::Config)
    ));

    let mut wrong_axis = bytes.clone();
    wrong_axis[SOURCE_SAMPLE_RATE_FIELD..SOURCE_SAMPLE_RATE_FIELD + 4]
        .copy_from_slice(&44_100_u32.to_le_bytes());
    assert!(matches!(
        AnalysisFile::parse(&wrong_axis, &fingerprint()),
        Err(AnalysisFileError::Corrupt)
    ));

    let mut wrong_extent = bytes.clone();
    wrong_extent[EXTENT_FIELD..EXTENT_FIELD + 8]
        .copy_from_slice(&(consts::ARCHIVE_EXTENT + consts::ARCHIVE_CHUNK_FRAMES).to_le_bytes());
    assert!(matches!(
        AnalysisFile::parse(&wrong_extent, &fingerprint()),
        Err(AnalysisFileError::Corrupt)
    ));

    let mut wrong_chunk_size = bytes.clone();
    wrong_chunk_size[CHUNK_FRAMES_FIELD..CHUNK_FRAMES_FIELD + 8]
        .copy_from_slice(&8_u64.to_le_bytes());
    assert!(matches!(
        AnalysisFile::parse(&wrong_chunk_size, &fingerprint()),
        Err(AnalysisFileError::Corrupt)
    ));
}

#[kithara::test]
fn update_rejects_axis_extent_and_fingerprint_drift() {
    let bytes = create_bytes(analysis(1, &[(0, 16)], false));
    let file = AnalysisFile::parse(&bytes, &fingerprint()).expect("file restores");
    let wrong_snapshots = [
        analysis_with(
            2,
            rate(44_100),
            Some(consts::ARCHIVE_EXTENT),
            fingerprint(),
            &[(0, 16)],
            false,
        ),
        analysis_with(
            2,
            rate(48_000),
            Some(consts::ARCHIVE_EXTENT + 1),
            fingerprint(),
            &[(0, 16)],
            false,
        ),
        analysis_with(
            2,
            rate(48_000),
            Some(consts::ARCHIVE_EXTENT),
            AnalysisFingerprint::new(Some("beat:other"), Some("wave:other")),
            &[(0, 16)],
            false,
        ),
    ];

    for wrong in &wrong_snapshots {
        let wrong = default_progress(wrong.clone());
        assert!(matches!(
            file.update(&wrong),
            Err(AnalysisFileError::Config)
        ));
    }
}

#[kithara::test]
fn update_rejects_completed_chunk_regression() {
    let bytes = create_bytes(analysis(1, &[(0, 32)], false));
    let file = AnalysisFile::parse(&bytes, &fingerprint()).expect("file restores");

    let regressed = default_progress(analysis(2, &[(16, 32)], false));
    assert!(matches!(
        file.update(&regressed),
        Err(AnalysisFileError::CoverageRegression { chunk: 0 })
    ));
}

#[kithara::test]
fn parser_rejects_truncation_corrupt_offsets_and_index_flags() {
    const PAYLOAD_END_FIELD: usize = 64;
    const LATEST_PAYLOAD_OFFSET_FIELD: usize = 72;

    let bytes = create_bytes(analysis(1, &[(0, 16)], false));
    for cut in [0, consts::HEADER_LEN - 1, bytes.len() - 1] {
        assert!(matches!(
            AnalysisFile::parse(&bytes[..cut], &fingerprint()),
            Err(AnalysisFileError::Corrupt)
        ));
    }

    let mut corrupt_latest = bytes.clone();
    corrupt_latest[LATEST_PAYLOAD_OFFSET_FIELD..LATEST_PAYLOAD_OFFSET_FIELD + 8]
        .copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        AnalysisFile::parse(&corrupt_latest, &fingerprint()),
        Err(AnalysisFileError::Corrupt)
    ));

    let mut invalid_entry = bytes.clone();
    invalid_entry[consts::HEADER_LEN] = 2;
    assert!(matches!(
        AnalysisFile::parse(&invalid_entry, &fingerprint()),
        Err(AnalysisFileError::Corrupt)
    ));

    let mut trailing = bytes.clone();
    trailing.push(0);
    let trailing_len = u64::try_from(trailing.len()).expect("fixture length fits u64");
    trailing[PAYLOAD_END_FIELD..PAYLOAD_END_FIELD + 8].copy_from_slice(&trailing_len.to_le_bytes());
    assert!(matches!(
        AnalysisFile::parse(&trailing, &fingerprint()),
        Err(AnalysisFileError::Corrupt)
    ));
}

#[kithara::test]
fn parser_rejects_index_coverage_disagreement() {
    let mut bytes = create_bytes(analysis(1, &[(0, 16)], false));
    bytes[consts::HEADER_LEN..consts::HEADER_LEN + consts::INDEX_ENTRY_LEN].fill(0);

    assert!(matches!(
        AnalysisFile::parse(&bytes, &fingerprint()),
        Err(AnalysisFileError::Corrupt)
    ));
}
