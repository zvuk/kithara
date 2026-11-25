use std::num::NonZeroU32;

use kithara_platform::{CancelScope, CancelToken};
use kithara_resampler::NoResamplerBackend;
use kithara_test_fixtures::analysis_fixtures::analysis_pcm;
use kithara_test_utils::kithara;
use kithara_worker::{Worker, WorkerConfig};

use super::{
    super::{
        AnalysisDemand,
        analyzer::AnalyzerBuilder,
        worker::{AnalysisWorker, AnalysisWorkerConfig},
    },
    fixtures::{FakeReader, sine},
};
use crate::test_pools::{Pools, TestPools, pools};

fn waveform_only(pools: Pools) -> AnalyzerBuilder<NoResamplerBackend, TestPools> {
    AnalyzerBuilder::<NoResamplerBackend, _>::new(pools).with_waveform(NonZeroU32::MIN)
}

fn worker(pools: Pools, parent: &CancelToken) -> AnalysisWorker {
    AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(waveform_only(pools))
            .cancel(parent.clone())
            .build(),
    )
}

#[kithara::test(tokio)]
async fn a_reader_wake_reticks_a_pending_pass_before_its_park_ends(analysis_pcm: &'static [f32]) {
    use kithara_platform::time::{self, Duration};
    let pools = pools();
    let worker = AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(waveform_only(pools.clone()))
            .wait_timeout(Duration::from_secs(60))
            .idle_timeout(Duration::from_secs(60))
            .build(),
    );
    let reader = FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)
        .pending_with_wake(worker.wake_handle());
    let (mut rx, _producer, _completion) = worker
        .analyze(
            Box::new(reader),
            "wake-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("analysis pass");
    time::timeout(Duration::from_secs(1), rx.changed())
        .await
        .expect("reader wake must interrupt parking")
        .expect("analysis progress");
    assert!(
        rx.borrow()
            .as_ref()
            .is_some_and(|progress| progress.analysis().waveform().is_some())
    );
}

#[kithara::test(tokio)]
async fn delivers_result_on_its_own_thread(analysis_pcm: &'static [f32]) {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);
    let (mut rx, _producer, _completion) = worker
        .analyze(
            Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
            "test-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    rx.changed().await.expect("worker sends a result");
    assert!(
        rx.borrow()
            .as_ref()
            .is_some_and(|progress| progress.analysis().waveform().is_some())
    );
}

#[kithara::test(tokio)]
async fn a_reader_on_another_axis_contributes_nothing(analysis_pcm: &'static [f32]) {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);
    let axis = NonZeroU32::new(48_000).expect("test rate is non-zero");
    let (mut rx, _producer, _completion) = worker
        .analyze(
            Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
            "test-track".into(),
            axis,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");

    assert!(
        rx.changed().await.is_err(),
        "a reader decoding onto another axis has nothing to contribute"
    );
    assert!(rx.borrow().is_none());
}

#[kithara::test(tokio)]
async fn preempted_job_sends_nothing_and_next_job_runs(analysis_pcm: &'static [f32]) {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);

    let (mut stale_rx, _stale_producer, stale_pass) = worker
        .open(
            "stale-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    stale_pass.cancel_token().cancel();
    let _completion = worker.start(
        stale_pass,
        Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
    );

    let (mut live_rx, _live_producer, _completion) = worker
        .analyze(
            Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
            "live-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    live_rx.changed().await.expect("live job completes");
    assert!(live_rx.borrow().is_some());
    assert!(
        stale_rx.changed().await.is_err(),
        "preempted job must drop its sender without a result"
    );
    assert!(stale_rx.borrow().is_none());
}

#[kithara::test(tokio)]
async fn pending_job_does_not_block_an_independent_job(analysis_pcm: &'static [f32]) {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);

    let (_pending_rx, _pending_producer, _completion) = worker
        .analyze(
            Box::new(FakeReader::stalled(10_000)),
            "pending-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    let (mut live_rx, _live_producer, _completion) = worker
        .analyze(
            Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
            "live-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");

    kithara_platform::time::timeout(
        kithara_platform::time::Duration::from_secs(2),
        live_rx.changed(),
    )
    .await
    .expect("an independent analysis job must not wait behind a pending reader")
    .expect("live analysis job publishes a result");
    assert!(live_rx.borrow().is_some());
}

#[kithara::test(tokio)]
async fn a_pass_publishes_above_the_revision_its_caller_holds(analysis_pcm: &'static [f32]) {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);
    let (mut rx, _producer, pass) = worker
        .open(
            "test-track".into(),
            super::fixtures::spec().sample_rate,
            3,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    let _completion = worker.start(
        pass,
        Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
    );

    rx.changed().await.expect("worker sends a result");
    let revision = rx
        .borrow()
        .as_ref()
        .map(|progress| progress.analysis().revision());
    assert!(
        revision.is_some_and(|revision| revision > 3),
        "the first publication outranks the held revision: {revision:?}"
    );
}

#[kithara::test(tokio)]
async fn job_token_belongs_to_worker_scope() {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools, &master);
    let (_rx, _producer, pass) = worker
        .open(
            "scoped-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    let job = pass.cancel_token().clone();

    drop(worker);

    assert!(
        job.is_cancelled(),
        "dropping the worker must cancel tokens it creates for jobs"
    );
}

#[kithara::test]
fn shared_base_outlives_analysis_dispatcher_and_analysis_cancel_stays_local() {
    let pools = pools();
    let base = Worker::new(WorkerConfig::new());
    let cancel = CancelScope::new(None);
    let worker = AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(waveform_only(pools))
            .worker(base.clone())
            .cancel(cancel.token())
            .build(),
    );
    let (_rx, _producer, pass) = worker
        .open(
            "scoped-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    let job = pass.cancel_token().clone();

    cancel.cancel();

    assert!(job.is_cancelled());
    assert!(!base.is_cancelled());
    drop(worker);
    assert!(!base.is_cancelled());
}

#[kithara::test(tokio)]
async fn complete_pass_reports_complete(analysis_pcm: &'static [f32]) {
    let pools = pools();
    let worker = worker(pools.clone(), &CancelToken::root());
    let (rx, _producer, completion) = worker
        .analyze(
            Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
            "complete-track".into(),
            super::fixtures::spec().sample_rate,
            7,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    let crate::AnalysisOutcome::Complete(progress) = completion.outcome().await else {
        panic!("whole source coverage reports complete");
    };
    assert!(progress.analysis().is_settled());
    assert!(progress.analysis().is_complete());
    assert!(progress.analysis().revision() > 7);
    let last = rx.borrow().clone().expect("final snapshot is published");
    assert_eq!(last.analysis().revision(), progress.analysis().revision());
    assert_eq!(last.analysis().token(), progress.analysis().token());
    assert_eq!(last.analysis().coverage(), progress.analysis().coverage());
}

#[kithara::test(tokio)]
async fn reader_error_reports_failed_read() {
    let worker = worker(pools(), &CancelToken::root());
    let (_rx, _producer, completion) = worker
        .analyze(
            Box::new(FakeReader::failing()),
            "read-error-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    assert!(matches!(
        completion.outcome().await,
        crate::AnalysisOutcome::Failed {
            error: crate::AnalysisError::Read(_),
            last: None,
        }
    ));
}

#[kithara::test(tokio)]
async fn empty_source_reports_empty() {
    let worker = worker(pools(), &CancelToken::root());
    let (_rx, _producer, completion) = worker
        .analyze(
            Box::new(FakeReader::empty()),
            "empty-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    assert!(matches!(
        completion.outcome().await,
        crate::AnalysisOutcome::Failed {
            error: crate::AnalysisError::Empty,
            last: None,
        }
    ));
}

#[kithara::test(tokio)]
async fn cancelled_pass_reports_cancelled() {
    let worker = worker(pools(), &CancelToken::root());
    let (_rx, _producer, pass) = worker
        .open(
            "cancelled-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    pass.cancel_token().cancel();
    let completion = worker.start(pass, Box::new(FakeReader::empty()));
    assert!(matches!(
        completion.outcome().await,
        crate::AnalysisOutcome::Cancelled { last: None }
    ));
}

#[kithara::test(tokio)]
async fn refused_admission_reports_rejected() {
    let worker = AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(waveform_only(pools()))
            .capacity(std::num::NonZeroUsize::MIN)
            .build(),
    );
    let (_held_rx, _held_producer, _held_completion) = worker
        .analyze(
            Box::new(FakeReader::stalled(10_000)),
            "held-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the first pass opens");
    let (_rx, _producer, completion) = worker
        .analyze(
            Box::new(FakeReader::empty()),
            "rejected-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the second pass opens");
    assert!(matches!(
        completion.outcome().await,
        crate::AnalysisOutcome::Failed {
            error: crate::AnalysisError::Rejected(kithara_worker::TaskError::Capacity {
                capacity: 1
            }),
            last: None,
        }
    ));
}

#[kithara::test(tokio)]
async fn dropped_subscriber_still_reports(analysis_pcm: &'static [f32]) {
    let pools = pools();
    let worker = worker(pools.clone(), &CancelToken::root());
    let (rx, _producer, completion) = worker
        .analyze(
            Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
            "unsubscribed-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    drop(rx);
    assert!(matches!(
        completion.outcome().await,
        crate::AnalysisOutcome::Complete(_)
    ));
}

#[kithara::test(tokio)]
async fn dropping_the_completion_does_not_cancel_the_pass(analysis_pcm: &'static [f32]) {
    let pools = pools();
    let worker = worker(pools.clone(), &CancelToken::root());
    let (mut rx, _producer, completion) = worker
        .analyze(
            Box::new(FakeReader::chunked(&pools, sine(analysis_pcm, 8192), 3)),
            "dropped-completion-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    drop(completion);
    while rx.changed().await.is_ok() {}
    assert!(
        rx.borrow()
            .as_ref()
            .is_some_and(|progress| progress.analysis().is_settled())
    );
}

#[kithara::test(tokio)]
async fn dropping_the_worker_reports_cancelled() {
    let worker = worker(pools(), &CancelToken::root());
    let (_rx, _producer, completion) = worker
        .analyze(
            Box::new(FakeReader::stalled(10_000)),
            "worker-drop-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    drop(worker);
    assert!(matches!(
        completion.outcome().await,
        crate::AnalysisOutcome::Cancelled { last: None }
    ));
}

#[kithara::test]
fn a_shared_base_shutdown_reports_stopped() {
    let base = Worker::new(WorkerConfig::new());
    let worker = AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(waveform_only(pools()))
            .worker(base.clone())
            .build(),
    );
    let (_rx, _producer, pass) = worker
        .open(
            "base-stop-track".into(),
            super::fixtures::spec().sample_rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the pass opens");
    let cancel = pass.cancel_token().clone();
    let completion = worker.start(pass, Box::new(FakeReader::stalled(10_000)));
    base.shutdown();
    assert!(!cancel.is_cancelled());
    let outcome = kithara_platform::tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(completion.outcome());
    assert!(matches!(
        outcome,
        crate::AnalysisOutcome::Failed {
            error: crate::AnalysisError::Stopped,
            last: None,
        }
    ));
}
