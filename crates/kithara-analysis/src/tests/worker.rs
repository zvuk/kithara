use std::num::NonZeroU32;

use kithara_platform::{CancelScope, CancelToken};
use kithara_resampler::NoResamplerBackend;
use kithara_test_utils::kithara;
use kithara_worker::{Worker, WorkerConfig};

use super::{
    super::{
        analyzer::AnalyzerBuilder,
        worker::{AnalysisWorker, AnalysisWorkerConfig},
    },
    fixtures::{FakeReader, sine},
};
use crate::test_pools::{Pools, TestPools, pools};

fn waveform_only(pools: Pools) -> AnalyzerBuilder<NoResamplerBackend, TestPools> {
    AnalyzerBuilder::<NoResamplerBackend, _>::new(pools).with_waveform(16)
}

fn worker(pools: Pools, parent: &CancelToken) -> AnalysisWorker {
    AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(waveform_only(pools))
            .cancel(parent.clone())
            .build(),
    )
}

#[kithara::test(tokio)]
async fn delivers_result_on_its_own_thread() {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);
    let (mut rx, _producer) = worker.analyze(
        Box::new(FakeReader::chunked(&pools, &sine(8192), 3)),
        "test-track".into(),
        super::fixtures::spec().sample_rate,
        0,
    );
    rx.changed().await.expect("worker sends a result");
    assert!(
        rx.borrow()
            .as_ref()
            .is_some_and(|progress| progress.analysis().waveform().is_some())
    );
}

#[kithara::test(tokio)]
async fn a_reader_on_another_axis_contributes_nothing() {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);
    let axis = NonZeroU32::new(48_000).expect("test rate is non-zero");
    let (mut rx, _producer) = worker.analyze(
        Box::new(FakeReader::chunked(&pools, &sine(8192), 3)),
        "test-track".into(),
        axis,
        0,
    );

    assert!(
        rx.changed().await.is_err(),
        "a reader decoding onto another axis has nothing to contribute"
    );
    assert!(rx.borrow().is_none());
}

#[kithara::test(tokio)]
async fn preempted_job_sends_nothing_and_next_job_runs() {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);

    let (mut stale_rx, _stale_producer, stale_pass) =
        worker.open("stale-track".into(), super::fixtures::spec().sample_rate, 0);
    stale_pass.cancel_token().cancel();
    worker.start(
        stale_pass,
        Box::new(FakeReader::chunked(&pools, &sine(8192), 3)),
    );

    let (mut live_rx, _live_producer) = worker.analyze(
        Box::new(FakeReader::chunked(&pools, &sine(8192), 3)),
        "live-track".into(),
        super::fixtures::spec().sample_rate,
        0,
    );
    live_rx.changed().await.expect("live job completes");
    assert!(live_rx.borrow().is_some());
    assert!(
        stale_rx.changed().await.is_err(),
        "preempted job must drop its sender without a result"
    );
    assert!(stale_rx.borrow().is_none());
}

#[kithara::test(tokio)]
async fn pending_job_does_not_block_an_independent_job() {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);

    let (_pending_rx, _pending_producer) = worker.analyze(
        Box::new(FakeReader::stalled(10_000)),
        "pending-track".into(),
        super::fixtures::spec().sample_rate,
        0,
    );
    let (mut live_rx, _live_producer) = worker.analyze(
        Box::new(FakeReader::chunked(&pools, &sine(8192), 3)),
        "live-track".into(),
        super::fixtures::spec().sample_rate,
        0,
    );

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
async fn a_pass_publishes_above_the_revision_its_caller_holds() {
    let pools = pools();
    let master = CancelToken::root();
    let worker = worker(pools.clone(), &master);
    let (mut rx, _producer, pass) =
        worker.open("test-track".into(), super::fixtures::spec().sample_rate, 3);
    worker.start(pass, Box::new(FakeReader::chunked(&pools, &sine(8192), 3)));

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
    let (_rx, _producer, pass) = worker.open(
        "scoped-track".into(),
        super::fixtures::spec().sample_rate,
        0,
    );
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
    let (_rx, _producer, pass) = worker.open(
        "scoped-track".into(),
        super::fixtures::spec().sample_rate,
        0,
    );
    let job = pass.cancel_token().clone();

    cancel.cancel();

    assert!(job.is_cancelled());
    assert!(!base.is_cancelled());
    drop(worker);
    assert!(!base.is_cancelled());
}
