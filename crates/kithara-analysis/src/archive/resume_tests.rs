use std::num::{NonZeroU32, NonZeroU64};

use kithara_resampler::NoResamplerBackend;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_utils::kithara;

use super::{AnalysisFile, AnalysisFileSpec, AnalysisFileUpdate};
use crate::{
    AnalysisProgress, AnalyzerBuilder,
    analyzer::{Detector, Extent, Ingest, TrackAnalyzers},
    beat::{BeatDetectError, BeatDetector, BeatMark, GridParams, RawBeats},
    test_pools::{Pools, TestPools, pools, sample_buffer},
};

const CHANNELS: u16 = 2;
const CHUNK_FRAMES: u64 = 128;
const EXTENT: u64 = 4 * CHUNK_FRAMES;
const SAMPLE_RATE: u32 = 64;

struct FixtureDetector;

impl BeatDetector for FixtureDetector {
    fn detect(&self, _mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        Ok(RawBeats {
            beats: vec![BeatMark::at(0.25)],
            downbeats: vec![BeatMark::at(0.25)],
        })
    }
}

fn configured(pools: Pools) -> (AnalyzerBuilder<NoResamplerBackend, TestPools>, Detector) {
    let mut builder = AnalyzerBuilder::<NoResamplerBackend, _>::new(pools)
        .with_waveform(8)
        .with_beat_detector(Box::new(FixtureDetector), GridParams::default());
    let detector = builder
        .take_detector()
        .expect("fixture detector is configured");
    (builder, detector)
}

fn rate() -> NonZeroU32 {
    NonZeroU32::new(SAMPLE_RATE).expect("fixture sample rate is non-zero")
}

fn chunk_frames() -> NonZeroU64 {
    NonZeroU64::new(CHUNK_FRAMES).expect("fixture chunk is non-zero")
}

fn decoded(pools: &Pools, at: u64) -> AudioChunk {
    let mut samples =
        Vec::with_capacity(usize::try_from(CHUNK_FRAMES).unwrap_or(0) * usize::from(CHANNELS));
    for frame in at..at + CHUNK_FRAMES {
        let phase = std::f64::consts::TAU * frame as f64 / 17.0;
        let sample = (phase.sin() * 0.5) as f32;
        samples.extend([sample, sample]);
    }
    AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec {
                channels: CHANNELS,
                sample_rate: rate(),
            },
            frames: u32::try_from(CHUNK_FRAMES).unwrap_or(0),
            frame_offset: at,
            ..Default::default()
        },
        sample_buffer(pools, &samples),
    )
}

fn fold(
    pools: &Pools,
    analyzers: &mut TrackAnalyzers<NoResamplerBackend, TestPools>,
    detector: &mut Detector,
    at: u64,
) {
    assert_eq!(
        analyzers.push(&decoded(pools, at), &mut Extent::default(), Some(detector)),
        Ingest::Accepted,
        "a missing fixed chunk is folded once"
    );
}

fn persist(progress: &AnalysisProgress) -> Vec<u8> {
    let spec = AnalysisFileSpec::new(
        progress.analysis().source_sample_rate(),
        progress.analysis().extent().expect("planned extent"),
        chunk_frames(),
        progress.analysis().fingerprint().clone(),
    )
    .expect("fixture spec is valid");
    let update = AnalysisFile::create(&spec, progress).expect("checkpoint is persisted");
    apply(&update)
}

fn apply(update: &AnalysisFileUpdate) -> Vec<u8> {
    let mut bytes = update
        .initial_bytes()
        .expect("create has a fixed prefix")
        .to_vec();
    let payload = update.payload();
    let start = usize::try_from(payload.offset()).expect("fixture offset fits");
    let end = start + payload.bytes().len();
    bytes.resize(end, 0);
    bytes[start..end].copy_from_slice(payload.bytes());
    for patch in update.patches() {
        let start = usize::try_from(patch.offset()).expect("fixture offset fits");
        let end = start + patch.bytes().len();
        bytes[start..end].copy_from_slice(patch.bytes());
    }
    bytes.truncate(usize::try_from(update.final_len()).expect("fixture length fits"));
    bytes
}

fn finish(
    analyzers: &mut TrackAnalyzers<NoResamplerBackend, TestPools>,
    detector: &mut Detector,
) -> crate::TrackAnalysis {
    analyzers.snapshot(Some(detector), true, Some(EXTENT))
}

#[kithara::test(native, flash(false))]
fn archived_partial_resumes_without_decoding_completed_chunks() {
    let pools = pools();
    let seed = [0, 2 * CHUNK_FRAMES];
    let (builder, mut detector) = configured(pools.clone());
    let mut partial = builder
        .build(rate(), "resume-track".into(), 0)
        .expect("analysis buffers fit the test region");
    for at in seed {
        fold(&pools, &mut partial, &mut detector, at);
    }
    let progress = partial.progress(Some(&mut detector), false, chunk_frames(), Some(EXTENT));
    let partial_revision = progress.analysis().revision();

    let bytes = persist(&progress);
    let file = AnalysisFile::parse(&bytes, progress.analysis().fingerprint())
        .expect("checkpoint and completion index validate together");
    assert_eq!(
        file.latest()
            .analysis()
            .coverage()
            .runs()
            .iter()
            .map(|range| range.start())
            .collect::<Vec<_>>(),
        seed
    );

    let progress = file.into();
    let (builder, mut detector) = configured(pools.clone());
    let mut resumed = builder
        .restore(&progress, chunk_frames())
        .expect("active analyzer config restores the opaque state");
    let requested: Vec<u64> = resumed
        .coverage()
        .gaps(EXTENT)
        .into_iter()
        .map(|range| range.start())
        .collect();
    assert_eq!(requested, [CHUNK_FRAMES, 3 * CHUNK_FRAMES]);
    assert!(
        requested.iter().all(|at| !seed.contains(at)),
        "completed fixed chunks are never requested again"
    );
    for at in &requested {
        fold(&pools, &mut resumed, &mut detector, *at);
    }
    let resumed = finish(&mut resumed, &mut detector);

    let (builder, mut detector) = configured(pools.clone());
    let mut uninterrupted = builder
        .build(rate(), "resume-track".into(), 0)
        .expect("analysis buffers fit the test region");
    for at in seed.into_iter().chain(requested) {
        fold(&pools, &mut uninterrupted, &mut detector, at);
    }
    let uninterrupted = finish(&mut uninterrupted, &mut detector);

    assert!(resumed.revision() > partial_revision);
    assert_eq!(
        resumed.waveform().expect("resumed waveform").buckets(),
        uninterrupted
            .waveform()
            .expect("uninterrupted waveform")
            .buckets()
    );
    let resumed_beat = resumed.beat().expect("resumed beat").artifact();
    let uninterrupted_beat = uninterrupted.beat().expect("uninterrupted beat").artifact();
    assert_eq!(resumed_beat.beats(), uninterrupted_beat.beats());
    assert_eq!(
        resumed_beat.beat_confidence(),
        uninterrupted_beat.beat_confidence()
    );
    assert_eq!(resumed_beat.downbeats(), uninterrupted_beat.downbeats());
    assert_eq!(
        resumed_beat.downbeat_confidence(),
        uninterrupted_beat.downbeat_confidence()
    );
}
