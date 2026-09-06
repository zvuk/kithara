//! The reader outruns the detector: it reads until the beat pass holds all it
//! may, and the detector reads what is held only once the reader has nothing
//! left to do.

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::SampleBuffer;
use kithara_platform::{CancelToken, tokio::sync::watch};
use kithara_resampler::NoResamplerBackend;
use kithara_signal::AudioSpec;
use kithara_test_utils::kithara;
use kithara_worker::TickResult;

use super::{
    super::{
        analyzer::AnalyzerBuilder,
        producer::ring,
        worker::{AnalysisTask, AnalysisWorker, AnalysisWorkerConfig, Job},
    },
    fixtures::beat_detector,
    track::Track,
};
use crate::{
    AnalysisProgress, BeatAnalysisConfig, BeatSnapshot, BeatState, TrackAnalysis,
    beat::GridParams,
    coverage::FrameRange,
    slots::beat::detect,
    test_pools::{Pools, TestPools, pools},
};

struct Consts;

impl Consts {
    const RATE: u32 = 1000;
    const CHUNK_FRAMES: u64 = 200;
    const CHUNK_SECONDS: u32 = 16;
    const TICK_LIMIT: u64 = 1 << 20;
    const PATIENCE: u32 = 16;
    const BUCKETS: usize = 64;
    const HOG_FRAMES: usize = 8192;
}

fn rate() -> NonZeroU32 {
    NonZeroU32::new(Consts::RATE).expect("the test rate is non-zero")
}

fn spec() -> AudioSpec {
    AudioSpec::new(1, rate())
}

struct Livelock {
    ticks: u64,
}

fn builder(pools: Pools) -> AnalyzerBuilder<NoResamplerBackend, TestPools> {
    AnalyzerBuilder::<NoResamplerBackend, _>::new(pools)
        .with_waveform(Consts::BUCKETS)
        .with_beat_config(
            BeatAnalysisConfig::builder()
                .resampler_backend(NoResamplerBackend)
                .target_rate(Consts::RATE)
                .build(),
        )
        .with_beat_detector(beat_detector(), GridParams::default())
}

fn chunk_seconds() -> NonZeroU32 {
    NonZeroU32::new(Consts::CHUNK_SECONDS).expect("non-zero")
}

fn read_whole(track: Track) -> Result<TrackAnalysis, Livelock> {
    read_whole_with(pools(), track, &mut |_| {})
}

fn read_whole_with(
    pools: Pools,
    track: Track,
    on_full: &mut dyn FnMut(&Pools),
) -> Result<TrackAnalysis, Livelock> {
    run(pools, track, on_full, &mut |_| false).map(|progress| progress.analysis().clone())
}

fn run(
    pools: Pools,
    track: Track,
    on_full: &mut dyn FnMut(&Pools),
    stop: &mut dyn FnMut(&AnalysisProgress) -> bool,
) -> Result<AnalysisProgress, Livelock> {
    let mut builder = builder(pools.clone());
    let mut detector = builder.take_detector().expect("a detector is configured");
    let (tx, results) = watch::channel::<Option<AnalysisProgress>>(None);
    let (_writer, ingest) = ring::open_for(rate());
    let job = Job {
        reader: Box::new(track),
        cancel: CancelToken::root(),
        ingest,
        rate: rate(),
        token: "hold".into(),
        revision: 0,
        tx,
        resume: None,
    };
    let mut task = AnalysisTask::new(
        job,
        &builder,
        chunk_seconds(),
        NonZeroUsize::new(8).expect("non-zero"),
        NonZeroU32::new(5).expect("non-zero"),
    )
    .expect("a fresh pass needs no resume");

    let mut ticks = 0;
    let mut waited = 0;
    let mut filled = false;
    let mut results = results;
    loop {
        if task.is_ending() {
            while let Some(request) = task.prepare_detection() {
                task.apply_detection(detect(request, &mut detector));
            }
        }
        ticks += 1;
        assert!(ticks < Consts::TICK_LIMIT, "the pass reads without end");
        let result = task.tick(&builder, None);
        if matches!(results.has_changed(), Ok(true))
            && results
                .borrow_and_update()
                .as_ref()
                .is_some_and(|progress| stop(progress))
        {
            break;
        }
        match result {
            TickResult::Done => break,
            TickResult::Backpressured => {
                if !filled {
                    filled = true;
                    on_full(&pools);
                }
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
    let progress = results
        .borrow()
        .clone()
        .expect("the pass publishes what it covered");
    Ok(progress)
}

fn exhaust(pools: &Pools, hog: &mut Vec<SampleBuffer>) {
    while let Ok(buffer) = pools.get_with_len::<f32>(Consts::HOG_FRAMES) {
        hog.push(buffer);
    }
    hog.pop();
}

fn lost(analysis: &TrackAnalysis) -> u64 {
    analysis.beat().map_or(0, |beat| {
        beat.unanalysed().iter().map(|range| range.frames()).sum()
    })
}

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

#[kithara::test]
fn a_pass_that_cannot_feed_its_detector_reads_on() {
    let pools = pools();
    let track = Track::silence(pools.clone(), spec(), Consts::CHUNK_FRAMES, 330.0);
    let mut hog = Vec::new();
    let analysis = read_whole_with(pools.clone(), track, &mut |pools| exhaust(pools, &mut hog))
        .unwrap_or_else(|Livelock { ticks }| {
            panic!("the pass waits on a detector it cannot feed, after {ticks} ticks")
        });
    assert!(
        analysis.is_complete(),
        "the track must be covered: {:?}",
        analysis.missing()
    );
    assert!(
        analysis.waveform().is_some(),
        "the waveform does not depend on the detector"
    );
}

#[kithara::test]
fn a_source_that_ends_before_its_claimed_length_is_complete() {
    let track = Track::claiming(pools(), spec(), Consts::CHUNK_FRAMES, 40.0, 40.5);
    let delivered = track.frames();
    let analysis = read_whole(track).unwrap_or_else(|Livelock { ticks }| {
        panic!("the pass waits on a detector that has nothing to read, after {ticks} ticks")
    });
    assert_eq!(
        analysis.extent(),
        Some(delivered),
        "the extent is where the source ended, whatever it claimed"
    );
    assert!(
        analysis.is_complete(),
        "the track must be covered: {:?}",
        analysis.missing()
    );
    assert!(analysis.is_settled());
    assert_eq!(lost(&analysis), 0, "the beat pass took the whole track");
    assert_eq!(
        analysis.beat().map(BeatSnapshot::state),
        Some(BeatState::Final),
        "a grid over the whole track is final"
    );
}

fn claiming(pools: Pools) -> Track {
    Track::claiming(pools, spec(), Consts::CHUNK_FRAMES, 40.0, 40.5)
}

#[kithara::test(tokio)]
async fn a_checkpoint_past_the_end_resumes_on_a_source_claiming_more() {
    let pools = pools();
    let delivered = claiming(pools.clone()).frames();
    let checkpoint = run(
        pools.clone(),
        claiming(pools.clone()),
        &mut |_| {},
        &mut |progress| progress.is_resumable() && progress.analysis().extent() == Some(delivered),
    )
    .unwrap_or_else(|Livelock { ticks }| {
        panic!("the pass waits on its detector after {ticks} ticks")
    });
    assert!(
        checkpoint.is_resumable() && checkpoint.analysis().extent() == Some(delivered),
        "a checkpoint is published once the source proved where it ends: {:?}",
        checkpoint.analysis().extent()
    );
    assert!(
        !checkpoint.analysis().is_complete(),
        "a checkpoint with the front of the track still to read: {:?}",
        checkpoint.analysis().missing()
    );

    let master = CancelToken::root();
    let worker = AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(builder(pools.clone()))
            .cancel(master.clone())
            .chunk_seconds(chunk_seconds())
            .build(),
    );
    let (mut rx, _producer, pass) = worker
        .open_resume(checkpoint.clone())
        .expect("a checkpoint past the end is a valid resume");
    worker.start(pass, Box::new(claiming(pools)));

    while rx.changed().await.is_ok() {}
    let held = rx.borrow().clone().expect("the resume publishes");
    assert!(
        held.analysis().is_complete(),
        "the resume finished the track: {:?}",
        held.analysis().missing()
    );
    assert_eq!(held.analysis().extent(), Some(delivered));
    assert!(held.analysis().revision() > checkpoint.analysis().revision());
}

#[kithara::test]
fn a_source_that_cannot_deliver_its_head_is_settled_with_a_final_grid() {
    const PRIMING: u64 = 1105;
    let analysis = read_whole(Track::priming(
        pools(),
        spec(),
        Consts::CHUNK_FRAMES,
        40.0,
        PRIMING,
    ))
    .unwrap_or_else(|Livelock { ticks }| {
        panic!("the pass waits on a detector that has nothing to read, after {ticks} ticks")
    });
    assert!(analysis.is_settled(), "nothing reachable is left");
    assert_eq!(
        analysis.missing(),
        vec![FrameRange::new(0, PRIMING)],
        "the head the source cannot deliver is the only gap"
    );
    assert_eq!(
        analysis.beat().map(BeatSnapshot::state),
        Some(BeatState::Final),
        "the grid over everything the source gives is final"
    );
}
