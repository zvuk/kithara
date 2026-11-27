#![forbid(unsafe_code)]

use std::sync::mpsc;

use criterion::{BatchSize, Criterion, Throughput};
use kithara_worker::{
    Dispatcher, DispatcherConfig, Task, TaskConfig, TickResult, Worker, WorkerConfig,
};

const TICKS: usize = 4_096;

struct Countdown {
    completed: Option<mpsc::Sender<usize>>,
    remaining: usize,
}

impl Countdown {
    const fn new(completed: mpsc::Sender<usize>) -> Self {
        Self {
            completed: Some(completed),
            remaining: TICKS,
        }
    }
}

impl Task for Countdown {
    fn tick(&mut self) -> TickResult {
        if self.remaining > 0 {
            self.remaining -= 1;
            return TickResult::Progress;
        }

        if let Some(completed) = self.completed.take() {
            completed.send(TICKS).ok();
        }
        TickResult::Done
    }
}

struct Prepared {
    _worker: Worker,
    dispatcher: Dispatcher,
    completed: Option<mpsc::Sender<usize>>,
    completion: Option<mpsc::Receiver<usize>>,
}

fn prepare() -> Prepared {
    let worker = Worker::new(WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("kithara-worker-bench")
            .build(),
    );
    let (completed, completion) = mpsc::channel();
    Prepared {
        _worker: worker,
        dispatcher,
        completed: Some(completed),
        completion: Some(completion),
    }
}

fn register_and_complete(
    dispatcher: &Dispatcher,
    completed: mpsc::Sender<usize>,
    completion: &mpsc::Receiver<usize>,
) {
    let handle = dispatcher
        .register(TaskConfig::new(), move |_| Countdown::new(completed))
        .unwrap_or_else(|error| panic!("benchmark task was not admitted: {error}"));
    let ticks = completion
        .recv()
        .unwrap_or_else(|error| panic!("benchmark task did not complete: {error}"));
    assert_eq!(ticks, TICKS);
    std::hint::black_box(handle);
}

fn teardown(prepared: Prepared) {
    prepared.dispatcher.shutdown();
    drop(prepared);
}

#[metabench::benchmark(DISPATCHER_PROGRESS, "dispatcher", "register_wake_and_progress", gungraun_setup = prepare, gungraun_teardown = teardown)]
fn dispatcher_case(mut prepared: Prepared) -> Prepared {
    register_and_complete(
        &prepared.dispatcher,
        prepared.completed.take().expect("prepared sender"),
        &prepared.completion.take().expect("prepared receiver"),
    );
    prepared
}

fn criterion_benchmarks(c: &mut Criterion) {
    let prepared = prepare();
    {
        let mut group = c.benchmark_group("dispatcher");
        group.sample_size(50);
        group.throughput(Throughput::Elements(
            u64::try_from(TICKS).expect("tick count exceeds u64"),
        ));
        group.bench_function("register_wake_and_progress", |b| {
            b.iter_batched(
                mpsc::channel,
                |(completed, completion)| {
                    register_and_complete(&prepared.dispatcher, completed, &completion);
                },
                BatchSize::PerIteration,
            );
        });
        group.finish();
    }
    teardown(prepared);
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [DISPATCHER_PROGRESS],
    allocator = std::alloc::System,
);
