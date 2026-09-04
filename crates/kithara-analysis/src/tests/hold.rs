//! The reader outruns the detector: it reads until the beat pass holds all it
//! may, and the detector reads what is held only once the reader has nothing
//! left to do.

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_platform::{CancelToken, tokio::sync::watch};
use kithara_resampler::NoResamplerBackend;
use kithara_signal::AudioSpec;
use kithara_test_utils::kithara;
use kithara_worker::TickResult;

use super::{
    super::{
        analyzer::AnalyzerBuilder,
        producer::ring,
        worker::{AnalysisTask, Job},
    },
    fixtures::beat_detector,
    track::Track,
};
use crate::{
    AnalysisProgress, BeatAnalysisConfig, TrackAnalysis, beat::GridParams, slots::beat::detect,
    test_pools::pools,
};

struct Consts;

impl Consts {
    /// A source axis on which a whole track costs nothing to read.
    const RATE: u32 = 1000;
    /// A fifth of a second, the chunk the decoders deliver.
    const CHUNK_FRAMES: u64 = 200;
    const CHUNK_SECONDS: u32 = 16;
    const TICK_LIMIT: u64 = 1 << 20;
    /// Ticks the pass may wait with nothing for the detector to read.
    const PATIENCE: u32 = 16;
}

fn rate() -> NonZeroU32 {
    NonZeroU32::new(Consts::RATE).expect("the test rate is non-zero")
}

fn spec() -> AudioSpec {
    AudioSpec::new(1, rate())
}

/// The pass waited on the detector while the detector had nothing to read.
struct Livelock {
    ticks: u64,
}

/// Reads the track through the scheduler with the detector as the slow
/// consumer: it reads what the pass holds only once the reader waits on it.
fn read_whole(track: Track) -> Result<TrackAnalysis, Livelock> {
    let pools = pools();
    let mut builder = AnalyzerBuilder::<NoResamplerBackend, _>::new(pools.clone())
        .with_beat_config(
            BeatAnalysisConfig::builder()
                .resampler_backend(NoResamplerBackend)
                .target_rate(Consts::RATE)
                .build(),
        )
        .with_beat_detector(beat_detector(), GridParams::default());
    let mut detector = builder.take_detector().expect("a detector is configured");
    let (tx, results) = watch::channel::<Option<AnalysisProgress>>(None);
    let (_writer, ingest) = ring::open_for(rate());
    let job = Job {
        reader: Box::new(track),
        cancel: CancelToken::root(),
        ingest,
        rate: rate(),
        token: "hold".into(),
        tx,
        resume: None,
    };
    let mut task = AnalysisTask::new(
        job,
        &builder,
        NonZeroU32::new(Consts::CHUNK_SECONDS).expect("non-zero"),
        NonZeroUsize::new(8).expect("non-zero"),
        NonZeroU32::new(5).expect("non-zero"),
    )
    .expect("a fresh pass needs no resume");

    let mut ticks = 0;
    let mut waited = 0;
    loop {
        if task.is_ending() {
            while let Some(request) = task.prepare_detection() {
                task.apply_detection(detect(request, &mut detector));
            }
        }
        ticks += 1;
        assert!(ticks < Consts::TICK_LIMIT, "the pass reads without end");
        match task.tick(&builder, None) {
            TickResult::Done => break,
            TickResult::Backpressured => {
                if let Some(request) = task.prepare_detection() {
                    task.apply_detection(detect(request, &mut detector));
                    waited = 0;
                } else {
                    waited += 1;
                    if waited > Consts::PATIENCE {
                        return Err(Livelock { ticks });
                    }
                }
            }
            _ => waited = 0,
        }
    }
    let analysis = results
        .borrow()
        .as_ref()
        .map(|progress| progress.analysis().clone())
        .expect("the pass publishes what it covered");
    Ok(analysis)
}

fn lost(analysis: &TrackAnalysis) -> u64 {
    analysis.beat().map_or(0, |beat| {
        beat.unanalysed().iter().map(|range| range.frames()).sum()
    })
}

/// The length of a track that never reached its end, at the app's 16 s
/// schedule chunk and 30 s detector windows: the hold filled with four runs
/// none of which reached a full window.
#[kithara::test]
fn a_track_the_reader_outruns_the_detector_on_reaches_its_end() {
    let analysis = read_whole(Track::silence(
        pools(),
        spec(),
        Consts::CHUNK_FRAMES,
        266.06,
    ))
    .unwrap_or_else(|Livelock { ticks }| {
        panic!("the pass waits on a detector that has nothing to read, after {ticks} ticks")
    });
    assert!(
        analysis.is_complete(),
        "the track must be covered: {:?}",
        analysis.missing()
    );
    assert_eq!(
        lost(&analysis),
        0,
        "the beat pass must take the whole track"
    );
}

/// The start of a track is the last place the schedule visits, because it
/// aims at the middle of what is uncovered. A pass that may only continue a
/// run it already holds has no answer for a gap with nothing in front of it,
/// and must not call that gap done.
#[kithara::test]
fn a_gap_at_the_start_is_taken_rather_than_declared_covered() {
    let analysis = read_whole(Track::silence(pools(), spec(), Consts::CHUNK_FRAMES, 407.2))
        .unwrap_or_else(|Livelock { ticks }| {
            panic!("the pass waits on a detector that has nothing to read, after {ticks} ticks")
        });
    assert_eq!(
        lost(&analysis),
        0,
        "the start of the track must be taken: {:?}",
        analysis.beat().map(|beat| beat.unanalysed().to_vec())
    );
}
