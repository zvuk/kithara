use std::cell::RefCell;

use kithara_platform::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
    tokio::runtime::Builder,
};
use kithara_test_utils::kithara;

use crate::{
    Dispatcher, DispatcherConfig, Event, Observer, ShutdownError, Task, TaskConfig, TickResult,
    Worker, WorkerConfig,
};

thread_local! {
    static EXIT: RefCell<Option<HeldExit>> = const { RefCell::new(None) };
}

struct HeldExit {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl Drop for HeldExit {
    fn drop(&mut self) {
        self.entered.send(()).expect("TLS exit receiver");
        self.release.recv().expect("release TLS exit");
    }
}

struct InstallExit {
    exit: Option<HeldExit>,
    ready: mpsc::Sender<()>,
}

impl Task for InstallExit {
    fn tick(&mut self) -> TickResult {
        EXIT.with(|slot| *slot.borrow_mut() = self.exit.take());
        self.ready.send(()).expect("task ready receiver");
        TickResult::Done
    }
}

#[kithara::test(native, flash(false))]
fn explicit_completion_waits_for_the_scheduler_tls_destructor() {
    let worker = Worker::new(WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("held-scheduler-exit")
            .build(),
    );
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (ready, ready_rx) = mpsc::channel();
    let _task = dispatcher
        .register(TaskConfig::new(), move |_| InstallExit {
            exit: Some(HeldExit {
                entered,
                release: release_rx,
            }),
            ready,
        })
        .expect("install TLS owner");
    ready_rx.recv().expect("task ran");
    let (completed, completed_rx) = mpsc::channel();
    let closers: Vec<_> = (0..2)
        .map(|_| {
            let dispatcher = dispatcher.clone();
            let completed = completed.clone();
            thread::spawn_named("scheduler-closer", move || {
                completed
                    .send(dispatcher.shutdown_and_join())
                    .expect("completion receiver");
            })
        })
        .collect();
    entered_rx.recv().expect("TLS destructor entered");
    let premature = completed_rx.recv_timeout(Instant::now() + Duration::from_millis(20));
    release.send(()).expect("release held destructor");
    for closer in closers {
        closer.join().expect("closer joins");
    }
    assert!(
        matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)),
        "completion returned while the scheduler TLS destructor was still held"
    );
    for _ in 0..2 {
        assert_eq!(completed_rx.recv().expect("caller completion"), Ok(()));
    }
    assert_eq!(dispatcher.shutdown_and_join(), Ok(()));
}

struct JoinSelf {
    dispatcher: Dispatcher,
    completed: mpsc::Sender<Result<(), ShutdownError>>,
    ready: Option<mpsc::Sender<()>>,
}

impl Task for JoinSelf {
    fn tick(&mut self) -> TickResult {
        if let Some(ready) = self.ready.take() {
            ready.send(()).expect("task ready receiver");
        }
        TickResult::Waiting
    }

    fn on_cancel(&mut self) {
        self.completed
            .send(self.dispatcher.shutdown_and_join())
            .expect("self-join result receiver");
    }
}

#[kithara::test(native, flash(false))]
fn explicit_completion_refuses_self_join_and_keeps_sibling_dispatchers_alive() {
    let worker = Worker::new(WorkerConfig::new());
    let dispatcher = worker.dispatcher(DispatcherConfig::builder().name("self-join").build());
    let sibling = worker.dispatcher(DispatcherConfig::builder().name("sibling-join").build());
    let (completed, completed_rx) = mpsc::channel();
    let (ready, ready_rx) = mpsc::channel();
    let _task = dispatcher
        .register(TaskConfig::new(), {
            let dispatcher = dispatcher.clone();
            move |_| JoinSelf {
                dispatcher,
                completed,
                ready: Some(ready),
            }
        })
        .expect("self-join task");
    ready_rx.recv().expect("task entered its scheduler");
    assert_eq!(dispatcher.shutdown_and_join(), Ok(()));
    assert_eq!(
        completed_rx.recv().expect("self join returned"),
        Err(ShutdownError::CurrentThread)
    );
    assert!(!worker.is_cancelled());
    assert!(!sibling.is_cancelled());
    assert_eq!(sibling.shutdown_and_join(), Ok(()));
}

struct PanickingObserver;

impl Observer for PanickingObserver {
    fn on_event(&mut self, _: Event) {
        panic!("scheduler observer panic");
    }
}

#[kithara::test(native, flash(false))]
fn explicit_completion_preserves_the_scheduler_panic_for_later_callers() {
    let worker = Worker::new(WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("panicking-scheduler")
            .observer(PanickingObserver)
            .build(),
    );
    assert_eq!(dispatcher.shutdown_and_join(), Err(ShutdownError::Panicked));
    assert_eq!(dispatcher.shutdown_and_join(), Err(ShutdownError::Panicked));
}

#[kithara::test(native, flash(false))]
fn final_drop_keeps_the_runtime_native_join_backstop() {
    let mut builder = Builder::new_current_thread();
    builder.enable_time();
    let runtime = builder.build().expect("owning runtime");
    let worker = Worker::new(WorkerConfig::new().with_runtime(runtime.handle().clone()));
    let dispatcher = worker.dispatcher(DispatcherConfig::builder().name("backstop-exit").build());
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (ready, ready_rx) = mpsc::channel();
    let _task = dispatcher
        .register(TaskConfig::new(), move |_| InstallExit {
            exit: Some(HeldExit {
                entered,
                release: release_rx,
            }),
            ready,
        })
        .expect("install TLS owner");
    ready_rx.recv().expect("task installed its TLS destructor");
    let (dropped, dropped_rx) = mpsc::channel();
    let (finished, finished_rx) = mpsc::channel();
    let closer = thread::spawn_named("runtime-backstop-close", move || {
        drop(dispatcher);
        drop(worker);
        dropped.send(()).expect("drop receiver");
        drop(runtime);
        finished.send(()).expect("runtime completion receiver");
    });
    entered_rx.recv().expect("scheduler entered TLS teardown");
    let nonblocking_drop = dropped_rx.recv_timeout(Instant::now() + Duration::from_millis(20));
    let premature_runtime = finished_rx.recv_timeout(Instant::now() + Duration::from_millis(20));
    release.send(()).expect("release held TLS destructor");
    closer.join().expect("runtime owner joins");
    assert!(
        nonblocking_drop.is_ok(),
        "dispatcher Drop waited for TLS teardown"
    );
    assert!(
        matches!(premature_runtime, Err(mpsc::RecvTimeoutError::Timeout)),
        "runtime backstop returned before native TLS completion"
    );
}
