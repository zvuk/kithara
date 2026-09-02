#![forbid(unsafe_code)]

use std::{hint::black_box, sync::mpsc};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use kithara_worker::{DispatcherConfig, Task, TaskConfig, TickResult, Worker, WorkerConfig};

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

fn bench_dispatcher(c: &mut Criterion) {
    let worker = Worker::new(WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("kithara-worker-bench")
            .build(),
    );
    {
        let mut group = c.benchmark_group("dispatcher");
        group.sample_size(50);
        group.throughput(Throughput::Elements(
            u64::try_from(TICKS).unwrap_or_else(|_| panic!("tick count exceeds u64")),
        ));

        group.bench_function("register_wake_and_progress", |b| {
            b.iter_batched(
                mpsc::channel,
                |(completed, completion)| {
                    let handle = dispatcher
                        .register(TaskConfig::new(), move |_| Countdown::new(completed))
                        .unwrap_or_else(|error| panic!("benchmark task was not admitted: {error}"));
                    let ticks = completion
                        .recv()
                        .unwrap_or_else(|error| panic!("benchmark task did not complete: {error}"));
                    assert_eq!(ticks, TICKS);
                    black_box(handle)
                },
                BatchSize::PerIteration,
            );
        });

        group.finish();
    }
    dispatcher.shutdown();
}

criterion_group!(benches, bench_dispatcher);
criterion_main!(benches);
