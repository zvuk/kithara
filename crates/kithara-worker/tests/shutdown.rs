use std::num::NonZeroUsize;

use kithara_platform::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use kithara_test_utils::kithara;
use kithara_worker::{DispatcherConfig, OwnedPoolConfig, TaskConfig, Worker, WorkerConfig};

#[kithara::test(native, flash(false))]
fn final_worker_drop_joins_compute_even_with_retained_task_handles() {
    let worker = Worker::new(WorkerConfig::new().with_owned_pool(OwnedPoolConfig::new(
        NonZeroUsize::new(2).expect("two compute threads"),
        "owned-shutdown",
    )));
    let dispatcher = worker.dispatcher(DispatcherConfig::builder().name("owned-scheduler").build());
    let pending = dispatcher.reserve(TaskConfig::new()).expect("reservation");
    let context = pending.context().clone();
    let (started, started_rx) = mpsc::channel();
    let (cancelled, cancelled_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    context
        .submit_compute((), move |compute, ()| {
            let _cancel = compute.token().on_cancel(move || {
                cancelled.send(()).ok();
            });
            started.send(()).ok();
            release_rx.recv().ok();
        })
        .expect("compute admitted");
    started_rx
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("compute started");
    let (returned, returned_rx) = mpsc::channel();
    let closer = thread::spawn(move || {
        drop(worker);
        returned.send(()).expect("owner returned");
    });
    let cancelled = cancelled_rx.recv_timeout(Instant::now() + Duration::from_secs(2));
    let premature = returned_rx.recv_timeout(Instant::now() + Duration::from_millis(50));
    drop(release);
    closer.join().expect("worker owner joined");
    cancelled.expect("worker cancellation reaches compute");
    assert!(
        premature.is_err(),
        "final Worker drop returned before owned compute cleanup"
    );
    returned_rx
        .try_recv()
        .expect("owner returned after cleanup");
    assert!(dispatcher.is_cancelled());
    assert!(context.token().is_cancelled());
}
