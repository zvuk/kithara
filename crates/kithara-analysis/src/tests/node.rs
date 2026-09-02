use std::num::{NonZeroU32, NonZeroUsize};

#[cfg(feature = "analysis-waveform")]
use kithara_audio::AudioObserveError;
use kithara_audio::AudioReader;
use kithara_bufpool::HasPool;
#[cfg(feature = "analysis-beat")]
use kithara_platform::sync::Arc;
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
use kithara_platform::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use kithara_platform::{CancelToken, sync::mpsc, tokio::sync::watch};
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
use kithara_platform::{
    thread,
    time::{Duration, Instant},
};
#[cfg(feature = "analysis-waveform")]
use kithara_resampler::NoResamplerBackend;
use kithara_resampler::ResamplerBackend;
#[cfg(feature = "analysis-beat")]
use kithara_resampler::rubato::RubatoBackend;
#[cfg(feature = "analysis-waveform")]
use kithara_signal::AudioSpec;
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
use kithara_test_utils::kithara;
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
use kithara_worker::TaskContext;
use kithara_worker::{
    Dispatcher, DispatcherConfig, PendingTask, RayonConfig, Task, TaskConfig, TickResult, Worker,
    WorkerConfig,
};
#[cfg(feature = "analysis-beat")]
use num_traits::cast::ToPrimitive;
#[cfg(feature = "analysis-beat")]
use unimock::{MockFn, Unimock, matching};

#[cfg(feature = "analysis-beat")]
use super::super::beat::{BeatDetector, BeatDetectorMock, BeatMark, GridParams, RawBeats};
#[cfg(feature = "analysis-waveform")]
use super::fixtures::CH;
use super::{
    super::{
        analyzer::AnalyzerBuilder,
        worker::{AnalysisNode, Job},
    },
    fixtures::{FakeReader, SR, sine},
};
#[cfg(feature = "analysis-beat")]
use crate::BeatState;
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
use crate::analyzer::BeatAnalysisConfig;
#[cfg(feature = "analysis-waveform")]
use crate::blob::to_bytes;
#[cfg(feature = "analysis-waveform")]
use crate::coverage::FrameRange;
#[cfg(feature = "analysis-waveform")]
use crate::producer::{AnalysisProducer, ring};
#[cfg(feature = "analysis-waveform")]
use crate::waveform::{AnalysisParams, WaveformAnalyzer};
#[cfg(not(feature = "analysis-waveform"))]
use crate::{AnalysisProgress, TrackAnalysis};
#[cfg(feature = "analysis-waveform")]
use crate::{
    AnalysisProgress, TrackAnalysis,
    test_pools::{TestPools, pools},
};

#[cfg(feature = "analysis-waveform")]
const BUCKETS: usize = 64;

pub(super) struct NodeHarness<B, S>
where
    B: ResamplerBackend,
{
    node: AnalysisNode<B, S>,
    _pending: PendingTask,
    _dispatcher: Dispatcher,
    _worker: Worker,
}

impl<B, S> NodeHarness<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub(super) fn new(builder: AnalyzerBuilder<B, S>, jobs: mpsc::Receiver<Job>) -> Self {
        Self::with_settings(
            builder,
            jobs,
            NonZeroU32::new(16).expect("test chunk duration is non-zero"),
            NonZeroUsize::new(8).expect("test drain limit is non-zero"),
            NonZeroU32::new(5).expect("test publish duration is non-zero"),
        )
    }

    pub(super) fn with_settings(
        builder: AnalyzerBuilder<B, S>,
        jobs: mpsc::Receiver<Job>,
        chunk_seconds: NonZeroU32,
        producer_drain_limit: NonZeroUsize,
        publish_seconds: NonZeroU32,
    ) -> Self {
        let compute_tasks = NonZeroUsize::MIN;
        let worker = Worker::new(
            WorkerConfig::new()
                .with_max_compute_tasks(compute_tasks)
                .with_owned_pool(RayonConfig::new(compute_tasks, "analysis-node-test")),
        );
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("analysis-node-test")
                .build(),
        );
        let pending = dispatcher
            .reserve(TaskConfig::new().with_max_compute_tasks(compute_tasks))
            .expect("test analysis task is reserved");
        let context = pending.context().clone();
        let node = AnalysisNode::new(
            builder,
            jobs,
            context.clone(),
            chunk_seconds,
            producer_drain_limit,
            publish_seconds,
        );

        Self {
            node,
            _pending: pending,
            _dispatcher: dispatcher,
            _worker: worker,
        }
    }

    #[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
    fn context(&self) -> &TaskContext {
        self._pending.context()
    }

    pub(super) fn tick(&mut self) -> TickResult {
        self.node.tick()
    }
}

#[cfg(feature = "analysis-waveform")]
fn waveform_only() -> AnalyzerBuilder<NoResamplerBackend, TestPools> {
    AnalyzerBuilder::<NoResamplerBackend, _>::new(pools()).with_waveform(BUCKETS)
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
fn beat_waveform(
    detector: Box<dyn BeatDetector>,
    min_window_seconds: u32,
    window_seconds: u32,
) -> AnalyzerBuilder<NoResamplerBackend, TestPools> {
    let config = BeatAnalysisConfig::builder()
        .resampler_backend(NoResamplerBackend)
        .target_rate(SR)
        .detector_min_window_seconds(min_window_seconds)
        .detector_window_seconds(window_seconds)
        .detector_overlap_seconds(0)
        .build();
    AnalyzerBuilder::<NoResamplerBackend, _>::new(pools())
        .with_beat_config(config)
        .with_beat_detector(detector, GridParams::default())
        .with_waveform(BUCKETS)
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
fn raw_beat(at: f32) -> RawBeats {
    RawBeats {
        beats: vec![BeatMark::at(at)],
        downbeats: vec![BeatMark::at(at)],
    }
}

#[cfg(feature = "analysis-waveform")]
fn enqueue(
    jobs: &mpsc::Sender<Job>,
    token: &str,
    reader: Box<dyn AudioReader>,
    cancel: CancelToken,
    ingest: ring::Reader,
) -> watch::Receiver<Option<AnalysisProgress>> {
    let (tx, results) = watch::channel(None);
    jobs.send(Job {
        token: token.into(),
        tx,
        rate: super::fixtures::spec().sample_rate,
        ingest,
        reader,
        cancel,
        resume: None,
    })
    .expect("analysis node accepts the test job");
    results
}

fn latest_analysis(results: &watch::Receiver<Option<AnalysisProgress>>) -> Option<TrackAnalysis> {
    results
        .borrow()
        .as_ref()
        .map(|progress| progress.analysis().clone())
}

fn take_analysis(results: &mut watch::Receiver<Option<AnalysisProgress>>) -> Option<TrackAnalysis> {
    results
        .borrow_and_update()
        .as_ref()
        .map(|progress| progress.analysis().clone())
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
fn drive_until<B, S>(node: &mut NodeHarness<B, S>, mut done: impl FnMut() -> bool)
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    let deadline = Instant::now() + Duration::from_secs(2);
    while !done() {
        assert!(Instant::now() < deadline, "analysis node made no progress");
        let _ = node.tick();
        thread::yield_now();
    }
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn pending_reader_yields_one_scheduler_tick() {
    let builder = waveform_only();
    let (jobs, receiver) = mpsc::channel();
    let (tx, _results) = watch::channel(None);
    jobs.send(Job {
        token: "test-track".into(),
        tx,
        rate: super::fixtures::spec().sample_rate,
        ingest: super::fixtures::idle_ingest(),
        reader: Box::new(FakeReader::chunked_with_pending(
            builder.pools(),
            &sine(1024),
            1,
        )),
        cancel: CancelToken::root(),
        resume: None,
    })
    .expect("analysis node accepts the test job");
    let mut node = NodeHarness::new(builder, receiver);

    assert_eq!(node.tick(), TickResult::UpstreamPending);
    assert_eq!(node.tick(), TickResult::Progress);
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn cancel_racing_finalize_publishes_partial_before_dropping_sender() {
    let builder = waveform_only();
    let (jobs, receiver) = mpsc::channel();
    let (tx, results) = watch::channel(None);
    let cancel = CancelToken::root();
    jobs.send(Job {
        token: "test-track".into(),
        tx,
        rate: super::fixtures::spec().sample_rate,
        ingest: super::fixtures::idle_ingest(),
        reader: Box::new(FakeReader::chunked(builder.pools(), &sine(1024), 1)),
        cancel: cancel.clone(),
        resume: None,
    })
    .expect("analysis node accepts the test job");
    let mut node = NodeHarness::new(builder, receiver);

    assert_eq!(node.tick(), TickResult::Progress, "decode one chunk");
    cancel.cancel();
    assert_eq!(node.tick(), TickResult::Progress, "cancel arms finalize");
    assert_eq!(node.tick(), TickResult::Progress, "finalize publishes");
    let snapshot = latest_analysis(&results).expect("covered audio is retained");
    assert!(!snapshot.is_settled(), "a cancelled pass remains resumable");
    assert!(results.has_changed().is_err(), "task sender is dropped");
}

#[cfg(feature = "analysis-waveform")]
fn offered(ranges: &[(u64, usize)]) -> Option<TrackAnalysis> {
    let rate = super::fixtures::spec().sample_rate;
    let (jobs, receiver) = mpsc::channel();
    let (tx, results) = watch::channel(None);
    let (writer, ingest) = ring::open_for(rate);
    let mut producer = AnalysisProducer::new(writer, rate, "test-track".into());
    jobs.send(Job {
        token: "test-track".into(),
        reader: Box::new(FakeReader::stalled(ranges.len() + 2)),
        tx,
        rate,
        ingest,
        cancel: CancelToken::root(),
        resume: None,
    })
    .expect("analysis node accepts the test job");
    let mut node = NodeHarness::new(waveform_only(), receiver);

    for (at, frames) in ranges {
        assert_eq!(
            producer.offer(&sine(*frames), super::fixtures::spec(), *at),
            Ok(()),
            "the transport takes a range on its own axis"
        );
    }

    for _ in 0..128 {
        let _ = node.tick();
    }
    // The watch keeps the last publication even once the task's sender is
    // gone, so this reads the final snapshot either way.
    latest_analysis(&results)
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn offered_ranges_land_where_they_were_offered() {
    let analysis = offered(&[(0, 1024), (4096, 1024)]).expect("the pass publishes");

    assert_eq!(
        analysis.coverage().runs(),
        &[FrameRange::new(0, 1024), FrameRange::new(4096, 1024)],
        "coverage is what was offered, at the positions it was offered at"
    );
    assert!(
        analysis.waveform().is_some(),
        "a pass fed only by a producer still produces artifacts"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn an_offer_reaches_only_the_pass_its_handle_names() {
    let rate = super::fixtures::spec().sample_rate;
    let open = |token: &str| {
        let (jobs, receiver) = mpsc::channel();
        let (tx, results) = watch::channel(None);
        let (writer, ingest) = ring::open_for(rate);
        jobs.send(Job {
            token: token.into(),
            reader: Box::new(FakeReader::stalled(8)),
            tx,
            rate,
            ingest,
            cancel: CancelToken::root(),
            resume: None,
        })
        .expect("analysis node accepts the test job");
        (
            jobs,
            NodeHarness::new(waveform_only(), receiver),
            results,
            AnalysisProducer::new(writer, rate, token.into()),
        )
    };

    let (_fed_jobs, mut fed_node, fed_results, mut producer) = open("track-a");
    let (_idle_jobs, mut idle_node, idle_results, _idle_producer) = open("track-b");

    assert_eq!(
        producer.offer(&sine(1024), super::fixtures::spec(), 0),
        Ok(())
    );
    for _ in 0..64 {
        let _ = fed_node.tick();
        let _ = idle_node.tick();
    }

    let fed = latest_analysis(&fed_results).expect("the fed pass publishes");
    assert_eq!(fed.token().as_str(), "track-a");
    assert_eq!(
        fed.coverage().runs(),
        &[FrameRange::new(0, 1024)],
        "the pass the handle names covers what it was offered"
    );
    assert!(
        idle_results.borrow().is_none(),
        "the pass no one offered to covers nothing and publishes nothing"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn an_offer_on_another_axis_leaves_the_coverage_alone() {
    let rate = super::fixtures::spec().sample_rate;
    let foreign = AudioSpec {
        channels: CH,
        sample_rate: NonZeroU32::new(48_000).expect("test rate is non-zero"),
    };
    let (jobs, receiver) = mpsc::channel();
    let (tx, results) = watch::channel(None);
    let (writer, ingest) = ring::open_for(rate);
    let mut producer = AnalysisProducer::new(writer, rate, "test-track".into());
    jobs.send(Job {
        token: "test-track".into(),
        reader: Box::new(FakeReader::stalled(4)),
        tx,
        rate,
        ingest,
        cancel: CancelToken::root(),
        resume: None,
    })
    .expect("analysis node accepts the test job");
    let mut node = NodeHarness::new(waveform_only(), receiver);

    assert_eq!(
        producer.offer(&sine(1024), super::fixtures::spec(), 0),
        Ok(())
    );
    assert_eq!(
        producer.offer(&sine(1024), foreign, 4096),
        Err(AudioObserveError::UnsupportedSampleRate {
            expected: rate,
            actual: foreign.sample_rate,
        }),
        "the mismatch is reported to the producer"
    );

    for _ in 0..128 {
        let _ = node.tick();
    }
    let analysis = latest_analysis(&results).expect("the pass publishes");
    assert_eq!(
        analysis.coverage().runs(),
        &[FrameRange::new(0, 1024)],
        "only the range on the pass's own axis is covered"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn a_pass_fed_by_a_producer_publishes_as_it_goes() {
    const BLOCK: u64 = 8192;
    const BLOCKS: u64 = 90;
    const STALLS: usize = 400;

    let rate = super::fixtures::spec().sample_rate;
    let (jobs, receiver) = mpsc::channel();
    let (tx, mut results) = watch::channel(None);
    let (writer, ingest) = ring::open_for(rate);
    let mut producer = AnalysisProducer::new(writer, rate, "test-track".into());
    jobs.send(Job {
        token: "test-track".into(),
        reader: Box::new(FakeReader::stalled(STALLS)),
        tx,
        rate,
        ingest,
        cancel: CancelToken::root(),
        resume: None,
    })
    .expect("analysis node accepts the test job");
    let mut node = NodeHarness::new(waveform_only(), receiver);
    let pcm = sine(usize::try_from(BLOCK).unwrap_or(0));

    let mut published = Vec::new();
    let collect = |results: &mut watch::Receiver<Option<AnalysisProgress>>,
                   out: &mut Vec<TrackAnalysis>| {
        if results.has_changed().is_ok_and(|changed| changed)
            && let Some(analysis) = take_analysis(results)
        {
            out.push(analysis);
        }
    };

    for block in 0..BLOCKS {
        assert_eq!(
            producer.offer(&pcm, super::fixtures::spec(), block * BLOCK),
            Ok(()),
            "the worker keeps the transport drained"
        );
        for _ in 0..4 {
            let _ = node.tick();
            collect(&mut results, &mut published);
        }
    }
    let mid = published.len();
    for _ in 0..STALLS {
        let _ = node.tick();
        collect(&mut results, &mut published);
    }
    // The task drops its sender the moment it finishes, so its last
    // publication is readable from the watch but never reported as a
    // change. Take it from the value itself.
    let last = latest_analysis(&results).expect("the pass publishes");
    if published
        .last()
        .is_none_or(|prev| prev.revision() < last.revision())
    {
        published.push(last.clone());
    }

    assert!(
        mid >= 2,
        "the pass publishes while coverage grows, not only at the end: {mid} before EOF"
    );
    assert!(
        published
            .windows(2)
            .all(|pair| pair[1].revision() > pair[0].revision()),
        "each publication outranks the last: {:?}",
        published
            .iter()
            .map(TrackAnalysis::revision)
            .collect::<Vec<_>>()
    );
    for early in published.iter().take(mid) {
        assert!(
            early.extent().is_none() && !early.is_complete(),
            "a publication made while coverage grows is provisional"
        );
        assert!(
            early.waveform().is_some(),
            "and it carries the artifact it is worth publishing for"
        );
    }
    assert_eq!(
        last.coverage().runs(),
        &[FrameRange::new(0, BLOCKS * BLOCK)],
        "everything offered is covered by the end: publications={}, revisions={:?}, missing={:?}",
        published.len(),
        published
            .iter()
            .map(TrackAnalysis::revision)
            .collect::<Vec<_>>(),
        last.missing()
    );
}

#[cfg(feature = "analysis-waveform")]
fn refusal_run(reoffer: bool) -> (TrackAnalysis, FrameRange, u64) {
    const BLOCK: u64 = 8192;
    const PAST: u64 = 40;
    /// Enough stalls that the reader outlives every offer below.
    const STALLS: usize = 200;

    let rate = super::fixtures::spec().sample_rate;
    let (jobs, receiver) = mpsc::channel();
    let (tx, results) = watch::channel(None);
    let (writer, ingest) = ring::open_for(rate);
    let mut producer = AnalysisProducer::new(writer, rate, "test-track".into());
    jobs.send(Job {
        token: "test-track".into(),
        reader: Box::new(FakeReader::stalled(STALLS)),
        tx,
        rate,
        ingest,
        cancel: CancelToken::root(),
        resume: None,
    })
    .expect("analysis node accepts the test job");
    let mut node = NodeHarness::new(waveform_only(), receiver);
    let pcm = sine(usize::try_from(BLOCK).unwrap_or(0));

    let mut at = 0;
    let refused = loop {
        match producer.offer(&pcm, super::fixtures::spec(), at) {
            Ok(()) => at = at.saturating_add(BLOCK),
            Err(AudioObserveError::Full) => break FrameRange::new(at, BLOCK),
            Err(other) => panic!("a range on the pass axis is taken or refused, got {other:?}"),
        }
    };

    for block in 1..PAST {
        for _ in 0..4 {
            let _ = node.tick();
        }
        assert_eq!(
            producer.offer(
                &pcm,
                super::fixtures::spec(),
                refused.start() + block * BLOCK
            ),
            Ok(()),
            "a drained transport takes the next range"
        );
    }
    if reoffer {
        for _ in 0..4 {
            let _ = node.tick();
        }
        assert_eq!(
            producer.offer(&pcm, super::fixtures::spec(), refused.start()),
            Ok(()),
            "the transport has room for the range it refused"
        );
    }
    for _ in 0..STALLS {
        let _ = node.tick();
    }

    let analysis = latest_analysis(&results).expect("the pass publishes");
    (analysis, refused, refused.start() + PAST * BLOCK)
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn a_range_the_transport_refused_is_reported_missing() {
    let (analysis, refused, reached) = refusal_run(false);

    assert!(
        analysis.missing().contains(&refused),
        "the refused range is missing: {refused:?} not in {:?}",
        analysis.missing()
    );
    assert!(
        !analysis.coverage().contains(refused),
        "a refused range is not covered"
    );
    assert_eq!(
        analysis.coverage().runs(),
        &[
            FrameRange::new(0, refused.start()),
            FrameRange::new(refused.end(), reached - refused.end()),
        ],
        "the hole splits the coverage in two"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn a_refused_range_offered_again_leaves_the_missing_set() {
    let (analysis, refused, reached) = refusal_run(true);

    assert!(
        analysis.missing().is_empty(),
        "the range was taken on the second offer: still missing {:?}",
        analysis.missing()
    );
    assert!(
        analysis.coverage().contains(refused),
        "and it is covered now"
    );
    assert_eq!(
        analysis.coverage().runs(),
        &[FrameRange::new(0, reached)],
        "entering the coverage once leaves one contiguous run"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn a_seek_order_pass_keeps_publishing_and_covers_the_union() {
    const BLOCK: u64 = 8192;
    const BLOCKS: u64 = 90;
    const STALLS: usize = 400;

    // The listener starts halfway in, then seeks back to the opening.
    let order: Vec<u64> = (BLOCKS / 2..BLOCKS).chain(0..BLOCKS / 2).collect();

    let rate = super::fixtures::spec().sample_rate;
    let (jobs, receiver) = mpsc::channel();
    let (tx, mut results) = watch::channel(None);
    let (writer, ingest) = ring::open_for(rate);
    let mut producer = AnalysisProducer::new(writer, rate, "test-track".into());
    jobs.send(Job {
        token: "test-track".into(),
        reader: Box::new(FakeReader::stalled(STALLS)),
        tx,
        rate,
        ingest,
        cancel: CancelToken::root(),
        resume: None,
    })
    .expect("analysis node accepts the test job");
    let mut node = NodeHarness::new(waveform_only(), receiver);
    let pcm = sine(usize::try_from(BLOCK).unwrap_or(0));

    let mut published: Vec<TrackAnalysis> = Vec::new();
    for block in &order {
        assert_eq!(
            producer.offer(&pcm, super::fixtures::spec(), block * BLOCK),
            Ok(()),
            "the worker keeps the transport drained"
        );
        for _ in 0..4 {
            let _ = node.tick();
            if results.has_changed().is_ok_and(|changed| changed)
                && let Some(analysis) = take_analysis(&mut results)
            {
                published.push(analysis);
            }
        }
    }
    for _ in 0..STALLS {
        let _ = node.tick();
    }
    let last = latest_analysis(&results).expect("the pass publishes");

    assert!(
        published.len() >= 2,
        "the pass publishes while coverage grows: {} publications",
        published.len()
    );
    assert!(
        published
            .windows(2)
            .all(|pair| pair[1].revision() > pair[0].revision()),
        "each publication outranks the last: {:?}",
        published
            .iter()
            .map(TrackAnalysis::revision)
            .collect::<Vec<_>>()
    );
    assert!(
        last.revision() > published.last().map_or(0, TrackAnalysis::revision),
        "and so does the last one"
    );
    assert!(
        published.first().is_some_and(|first| first
            .coverage()
            .runs()
            .first()
            .is_some_and(|run| run.start() > 0)),
        "the seek reached the pass: the first publication does not start at zero"
    );
    assert_eq!(
        last.coverage().runs(),
        &[FrameRange::new(0, BLOCKS * BLOCK)],
        "coverage is the union of everything offered"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn offers_out_of_order_cover_their_union() {
    let ascending = offered(&[(0, 1024), (1024, 1024), (2048, 1024)]);
    let shuffled = offered(&[(2048, 1024), (0, 1024), (1024, 1024)]);

    let ascending = ascending.expect("the ascending pass publishes");
    let shuffled = shuffled.expect("the shuffled pass publishes");
    assert_eq!(
        ascending.coverage().runs(),
        &[FrameRange::new(0, 3072)],
        "three touching ranges are one run"
    );
    assert_eq!(
        shuffled.coverage(),
        ascending.coverage(),
        "arrival order does not change what is covered"
    );
}

fn stages<B, S>(
    reader: Box<dyn AudioReader>,
    builder: AnalyzerBuilder<B, S>,
    cancel: &CancelToken,
) -> Vec<TrackAnalysis>
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    let (jobs, receiver) = mpsc::channel();
    let (tx, mut results) = watch::channel(None);
    jobs.send(Job {
        token: "test-track".into(),
        reader,
        tx,
        rate: super::fixtures::spec().sample_rate,
        ingest: super::fixtures::idle_ingest(),
        cancel: cancel.clone(),
        resume: None,
    })
    .expect("analysis node accepts the test job");
    let mut node = NodeHarness::new(builder, receiver);
    let mut out = Vec::new();
    for _ in 0..128 {
        let _ = node.tick();
        match results.has_changed() {
            Ok(true) => {
                if let Some(analysis) = take_analysis(&mut results) {
                    out.push(analysis);
                }
            }
            Ok(false) => {}
            Err(_) => {
                if let Some(analysis) = take_analysis(&mut results) {
                    out.push(analysis);
                }
                break;
            }
        }
    }
    out
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn matches_direct_waveform_analyzer_over_chunked_stream() {
    let samples = sine(usize::try_from(SR).unwrap());
    let frames = u64::try_from(samples.len() / usize::from(CH)).unwrap_or(0);
    let builder = waveform_only();
    let pools = builder.pools().clone();
    let mut direct = WaveformAnalyzer::new(SR, AnalysisParams::default(), &pools)
        .expect("waveform buffers fit the test region");
    direct
        .push(&pools, &samples, usize::from(CH), 0)
        .expect("waveform buffers fit the test region");
    let want = direct.snapshot(BUCKETS, Some(frames));

    let reader = Box::new(FakeReader::chunked(&pools, &samples, 4));
    let out = stages(reader, builder, &CancelToken::root());
    assert_eq!(out.len(), 1, "waveform-only emits once");
    let got = out[0]
        .waveform()
        .cloned()
        .expect("waveform analyzer fills its slot");
    assert_eq!(
        to_bytes(&got),
        to_bytes(&want),
        "worker path must reproduce the direct analyzer output"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn cancelled_token_yields_none() {
    let builder = waveform_only();
    let cancel = CancelToken::root();
    cancel.cancel();
    let reader = Box::new(FakeReader::chunked(builder.pools(), &sine(4096), 2));
    assert!(stages(reader, builder, &cancel).is_empty());
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn decode_error_yields_none() {
    let reader = Box::new(FakeReader::failing());
    let out = stages(reader, waveform_only(), &CancelToken::root());
    assert!(out.is_empty());
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn empty_stream_yields_none() {
    let reader = Box::new(FakeReader::empty());
    let out = stages(reader, waveform_only(), &CancelToken::root());
    assert!(out.is_empty(), "EOF with no chunks is not an analysis");
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
#[kithara::test(native, flash(false))]
fn a_slow_detector_does_not_stop_decoder_or_ring_progress() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_detector = Arc::clone(&calls);
    let (started, started_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let release_for_detector = Arc::clone(&release_rx);
    let detector = Box::new(Unimock::new(
        BeatDetectorMock
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, mono| {
                if calls_for_detector.fetch_add(1, Ordering::SeqCst) == 0 {
                    started.send(mono.len()).ok();
                    release_for_detector.lock().recv().ok();
                }
                Ok(raw_beat(0.5))
            })),
    ));
    let builder = beat_waveform(detector, 1, 1);
    let rate = super::fixtures::spec().sample_rate;
    let frames = usize::try_from(SR).expect("test rate fits usize");
    let (jobs, receiver) = mpsc::channel();
    let (writer, ingest) = ring::open_for(rate);
    let mut producer = AnalysisProducer::new(writer, rate, "same-track".into());
    let mut results = enqueue(
        &jobs,
        "same-track",
        Box::new(FakeReader::chunked(builder.pools(), &sine(3 * frames), 3)),
        CancelToken::root(),
        ingest,
    );
    let mut node = NodeHarness::with_settings(
        builder,
        receiver,
        NonZeroU32::new(16).expect("test chunk duration is non-zero"),
        NonZeroUsize::new(8).expect("test drain limit is non-zero"),
        NonZeroU32::MIN,
    );

    assert_eq!(node.tick(), TickResult::Progress, "first chunk is decoded");
    assert_eq!(
        started_rx
            .recv_timeout(Instant::now() + Duration::from_secs(2))
            .expect("detector starts"),
        frames,
        "one source second reaches the detector"
    );

    let offered_at = 2 * u64::from(SR);
    assert_eq!(
        producer.offer(&sine(frames), super::fixtures::spec(), offered_at),
        Ok(())
    );
    assert_eq!(
        node.tick(),
        TickResult::Progress,
        "decode and producer ingest continue while Rayon is occupied"
    );
    let snapshot = results
        .borrow_and_update()
        .clone()
        .expect("progress is published while detection is blocked");
    assert!(
        snapshot
            .analysis()
            .coverage()
            .contains(FrameRange::new(u64::from(SR), u64::from(SR))),
        "the decoder reached its second chunk"
    );
    assert!(
        snapshot
            .analysis()
            .coverage()
            .contains(FrameRange::new(offered_at, u64::from(SR))),
        "the playback ring was drained"
    );

    release.send(()).expect("release the slow detector");
    drive_until(&mut node, || results.has_changed().is_err());
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
#[kithara::test(native, flash(false))]
fn saturation_retries_the_exact_detection_payload_once() {
    let (detected, detected_rx) = mpsc::channel();
    let detector = Box::new(Unimock::new(
        BeatDetectorMock
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, mono| {
                detected.send(mono.to_vec()).ok();
                Ok(raw_beat(0.5))
            })),
    ));
    let builder = beat_waveform(detector, 1, 1);
    let frames = usize::try_from(SR).expect("test rate fits usize");
    let pcm = sine(frames);
    let expected: Vec<f32> = pcm
        .chunks_exact(usize::from(CH))
        .map(|frame| frame.iter().sum::<f32>() / f32::from(CH))
        .collect();
    let (jobs, receiver) = mpsc::channel();
    let results = enqueue(
        &jobs,
        "saturated-track",
        Box::new(FakeReader::chunked(builder.pools(), &pcm, 1)),
        CancelToken::root(),
        super::fixtures::idle_ingest(),
    );
    let mut node = NodeHarness::with_settings(
        builder,
        receiver,
        NonZeroU32::new(16).expect("test chunk duration is non-zero"),
        NonZeroUsize::new(8).expect("test drain limit is non-zero"),
        NonZeroU32::MIN,
    );
    let (blocked, blocked_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (finished, finished_rx) = mpsc::channel();
    node.context()
        .submit_compute((), move |_, ()| {
            blocked.send(()).ok();
            release_rx.recv().ok();
            finished.send(()).ok();
        })
        .expect("the blocker occupies the only compute budget");
    blocked_rx
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("compute budget is occupied");

    assert_eq!(node.tick(), TickResult::Progress, "the request is retained");
    assert!(
        detected_rx.try_recv().is_err(),
        "saturation does not execute the detector inline"
    );

    release.send(()).expect("release the compute budget");
    finished_rx
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("compute budget is released");
    let deadline = Instant::now() + Duration::from_secs(2);
    let detected = loop {
        let _ = node.tick();
        match detected_rx.try_recv() {
            Ok(detected) => break detected,
            Err(_) if Instant::now() < deadline => thread::yield_now(),
            Err(error) => panic!("the retained request was not retried: {error:?}"),
        }
    };
    assert_eq!(
        detected, expected,
        "the pooled detector input survives saturation unchanged"
    );
    drive_until(&mut node, || results.has_changed().is_err());
    assert!(
        detected_rx.try_recv().is_err(),
        "the retained request executes exactly once"
    );
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
#[kithara::test(native, flash(false))]
fn cancelled_late_result_cannot_contaminate_the_same_token_next_pass() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_detector = Arc::clone(&calls);
    let (called, called_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let release_for_detector = Arc::clone(&release_rx);
    let detector = Box::new(Unimock::new(
        BeatDetectorMock
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, _| {
                let call = calls_for_detector.fetch_add(1, Ordering::SeqCst);
                called.send(call).ok();
                if call == 0 {
                    release_for_detector.lock().recv().ok();
                    Ok(raw_beat(0.25))
                } else {
                    Ok(raw_beat(0.75))
                }
            })),
    ));
    let builder = beat_waveform(detector, 1, 1);
    let frames = usize::try_from(SR).expect("test rate fits usize");
    let (jobs, receiver) = mpsc::channel();
    let cancel_a = CancelToken::root();
    let mut results_a = enqueue(
        &jobs,
        "same-token",
        Box::new(FakeReader::chunked(builder.pools(), &sine(frames), 1)),
        cancel_a.clone(),
        super::fixtures::idle_ingest(),
    );
    let mut results_b = enqueue(
        &jobs,
        "same-token",
        Box::new(FakeReader::chunked(builder.pools(), &sine(frames), 1)),
        CancelToken::root(),
        super::fixtures::idle_ingest(),
    );
    let mut node = NodeHarness::new(builder, receiver);

    let _ = node.tick();
    assert_eq!(
        called_rx
            .recv_timeout(Instant::now() + Duration::from_secs(2))
            .expect("pass A detector starts"),
        0
    );
    cancel_a.cancel();
    let _ = node.tick();
    let _ = node.tick();
    assert!(
        take_analysis(&mut results_a).is_some_and(|snapshot| !snapshot.is_settled()),
        "cancelled A publishes its resumable partial"
    );
    let _ = node.tick();
    let _ = node.tick();
    assert!(
        called_rx.try_recv().is_err(),
        "B waits for ownership of the detector"
    );

    release.send(()).expect("let A return after B has started");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = node.tick();
        match called_rx.try_recv() {
            Ok(call) => {
                assert_eq!(call, 1, "the next detection belongs to B");
                break;
            }
            Err(_) if Instant::now() < deadline => thread::yield_now(),
            Err(error) => panic!("B detection did not start: {error:?}"),
        }
    }
    drive_until(&mut node, || results_b.has_changed().is_err());
    let snapshot = take_analysis(&mut results_b).expect("B publishes its final analysis");
    assert_eq!(snapshot.token().as_str(), "same-token");
    let beats = snapshot
        .beat()
        .expect("B detection fills the beat slot")
        .artifact()
        .beats();
    assert!(
        beats.contains(&(3 * u64::from(SR) / 4)),
        "B's marker is retained: {beats:?}"
    );
    assert!(
        !beats.contains(&(u64::from(SR) / 4)),
        "A's late marker cannot enter B: {beats:?}"
    );
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
#[kithara::test(native, flash(false))]
fn final_publication_waits_for_trailing_detection() {
    let (started, started_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let detector = Box::new(Unimock::new(
        BeatDetectorMock
            .next_call(matching!(_))
            .answers_arc(Arc::new(move |_, _| {
                started.send(()).ok();
                release_rx.lock().recv().ok();
                Ok(raw_beat(0.5))
            })),
    ));
    let builder = beat_waveform(detector, 2, 2);
    let frames = usize::try_from(SR).expect("test rate fits usize");
    let (jobs, receiver) = mpsc::channel();
    let mut results = enqueue(
        &jobs,
        "trailing-track",
        Box::new(FakeReader::chunked(builder.pools(), &sine(frames), 1)),
        CancelToken::root(),
        super::fixtures::idle_ingest(),
    );
    let mut node = NodeHarness::new(builder, receiver);

    let _ = node.tick();
    let _ = node.tick();
    started_rx
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("EOF starts the short trailing window");
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(
        results.borrow().is_none(),
        "nothing final is published early"
    );
    assert!(
        matches!(results.has_changed(), Ok(false)),
        "the final sender remains alive while detection runs"
    );

    release.send(()).expect("release trailing detection");
    drive_until(&mut node, || results.has_changed().is_err());
    assert!(
        results
            .borrow_and_update()
            .as_ref()
            .is_some_and(|snapshot| snapshot.analysis().beat().is_some()),
        "the trailing result rides the final publication"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test(native, flash(false))]
fn producer_drain_limit_bounds_one_tick() {
    let rate = super::fixtures::spec().sample_rate;
    let frames = usize::try_from(SR).expect("test rate fits usize");
    let (jobs, receiver) = mpsc::channel();
    let (writer, ingest) = ring::open_for(rate);
    let mut producer = AnalysisProducer::new(writer, rate, "drain-track".into());
    let mut results = enqueue(
        &jobs,
        "drain-track",
        Box::new(FakeReader::stalled(8)),
        CancelToken::root(),
        ingest,
    );
    let mut node = NodeHarness::with_settings(
        waveform_only(),
        receiver,
        NonZeroU32::new(16).expect("test chunk duration is non-zero"),
        NonZeroUsize::MIN,
        NonZeroU32::MIN,
    );
    let pcm = sine(frames);
    for block in 0..3u64 {
        assert_eq!(
            producer.offer(&pcm, super::fixtures::spec(), block * u64::from(SR)),
            Ok(())
        );
    }

    for expected in 1..=3u64 {
        assert_eq!(node.tick(), TickResult::Progress);
        let snapshot = results
            .borrow_and_update()
            .clone()
            .expect("each drained source second is published");
        assert_eq!(
            snapshot.analysis().coverage().runs(),
            &[FrameRange::new(0, expected * u64::from(SR))],
            "one tick drains exactly one descriptor"
        );
    }
}

#[cfg(feature = "analysis-beat")]
#[kithara::test]
fn beat_slot_fills_the_beat_grid() {
    let raw = RawBeats {
        beats: Vec::new(),
        downbeats: (0..9u8).map(|n| BeatMark::at(f32::from(n) * 2.0)).collect(),
    };
    let mock = Unimock::new(
        BeatDetectorMock
            .next_call(matching!(_))
            .answers_arc(Arc::new(move |_, _| Ok(raw.clone()))),
    );
    let detector = Box::new(mock) as Box<dyn BeatDetector>;
    let builder = AnalyzerBuilder::<RubatoBackend, _>::new(pools())
        .with_beat_detector(detector, GridParams::default());

    let reader = Box::new(FakeReader::chunked(
        builder.pools(),
        &sine(17 * usize::try_from(SR).unwrap()),
        3,
    ));
    let out = stages(reader, builder, &CancelToken::root());
    assert!(
        out.len() >= 2,
        "17 s of source outlives one publication interval, got {} publication(s)",
        out.len()
    );
    let revisions: Vec<u64> = out.iter().map(TrackAnalysis::revision).collect();
    assert!(
        revisions.windows(2).all(|pair| pair[1] > pair[0]),
        "each publication must outrank the last: {revisions:?}"
    );
    assert!(
        out.iter()
            .any(|analysis| analysis.beat().is_some() && analysis.extent().is_none()),
        "a grid must reach a consumer before the extent is known"
    );
    // Every stage between the detector and the snapshot can drop a marker's
    // confidence, and each is unit-tested on its own. This is the one place
    // that proves a mark survives all of them still carrying one.
    let marked = out
        .iter()
        .filter_map(TrackAnalysis::beat)
        .find(|beat| {
            beat.artifact()
                .beat_confidence()
                .iter()
                .chain(beat.artifact().downbeat_confidence())
                .any(Option::is_some)
        })
        .expect("some publication carries a marker the detector reported");
    assert_eq!(
        marked.confidence(),
        Some(0.9),
        "the number the detector reported survives every stage between it and \
         the published snapshot"
    );
    assert!(
        out.iter()
            .filter(|analysis| analysis.extent().is_none())
            .all(|analysis| analysis
                .beat()
                .is_none_or(|beat| beat.state() == BeatState::Provisional)),
        "a grid published mid-decode cannot claim to be final"
    );
    let last = out.last().expect("at least one publication");
    assert_eq!(
        last.extent(),
        Some(u64::from(SR) * 17),
        "end of stream pins the extent to what was covered"
    );
    let grid = last
        .beat()
        .cloned()
        .expect("beat slot fills its slot in the final publication");
    assert!(
        (grid.artifact().bpm() - 120.0).abs() < 1e-6,
        "2 s bars are 120 bpm, got {}",
        grid.artifact().bpm()
    );
    // The reported tempo must describe the markers riding the same
    // revision, not a value derived from something already replaced.
    let downbeats = grid.artifact().downbeats();
    let mut gaps: Vec<u64> = downbeats
        .windows(2)
        .filter_map(|pair| pair[1].checked_sub(pair[0]))
        .collect();
    gaps.sort_unstable();
    let bar_frames = gaps.get(gaps.len() / 2).copied().unwrap_or(0);
    let bar_seconds = bar_frames.to_f64().unwrap_or(1.0) / f64::from(SR);
    let bpm_from_marks = 4.0 * 60.0 / bar_seconds;
    assert!(
        (bpm_from_marks - grid.artifact().bpm()).abs() < 1e-6,
        "bpm {} must describe the published markers ({bpm_from_marks} from bars)",
        grid.artifact().bpm()
    );
    assert_eq!(
        grid.artifact().downbeats()[1],
        u64::from(SR) * 2,
        "source frames"
    );
}

#[cfg(feature = "analysis-waveform")]
#[kithara::test]
fn pending_is_tolerated_mid_stream() {
    let builder = waveform_only();
    let samples = sine(8192);
    let reader = Box::new(FakeReader::chunked_with_pending(
        builder.pools(),
        &samples,
        2,
    ));
    let out = stages(reader, builder, &CancelToken::root());
    assert!(out.len() == 1 && out[0].waveform().is_some());
}
