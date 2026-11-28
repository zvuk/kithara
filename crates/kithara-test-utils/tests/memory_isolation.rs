#![forbid(unsafe_code)]

use std::{
    hint::{black_box, spin_loop},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use kithara_test_utils::memory::{self, Counting};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measurement {
    held: Vec<u8>,
    subsystem: Vec<u8>,
    heap: memory::Heap,
    live: usize,
}

fn measure_worker_allocations(harness_bytes: usize, subsystem_bytes: usize) -> Measurement {
    const HELD_BYTES: usize = 4096;

    let start_harness = AtomicBool::new(false);
    let harness_allocated = AtomicBool::new(false);
    let finish_harness = AtomicBool::new(false);
    let subsystem_sent = AtomicBool::new(false);

    let (before, held, subsystem, heap, during) = thread::scope(|scope| {
        let harness = scope.spawn(|| {
            let _exclusion = memory::exclude_current_thread();
            while !start_harness.load(Ordering::Acquire) {
                spin_loop();
            }
            let foreign = black_box(vec![0_u8; harness_bytes]);
            harness_allocated.store(true, Ordering::Release);
            while !finish_harness.load(Ordering::Acquire) {
                spin_loop();
            }
            drop(foreign);
        });

        let before = memory::live_bytes();
        let ((held, subsystem), heap) = memory::measure(|| {
            let held = black_box(vec![0_u8; HELD_BYTES]);
            start_harness.store(true, Ordering::Release);
            while !harness_allocated.load(Ordering::Acquire) {
                spin_loop();
            }

            let (send, receive) = mpsc::channel();
            let subsystem_sent = &subsystem_sent;
            let subsystem_worker = scope.spawn(move || {
                let subsystem = black_box(vec![0_u8; subsystem_bytes]);
                send.send(subsystem)
                    .expect("the subsystem receiver is alive");
                subsystem_sent.store(true, Ordering::Release);
            });
            while !subsystem_sent.load(Ordering::Acquire) {
                spin_loop();
            }
            let subsystem = receive
                .try_recv()
                .expect("the subsystem worker sent its allocation");
            subsystem_worker
                .join()
                .expect("the subsystem worker must finish");
            (held, subsystem)
        });
        let during = memory::live_bytes();
        finish_harness.store(true, Ordering::Release);
        harness.join().expect("the harness worker must finish");
        (before, held, subsystem, heap, during)
    });

    Measurement {
        held,
        subsystem,
        heap,
        live: during.saturating_sub(before),
    }
}

#[test]
fn measure_excludes_harness_threads_and_counts_subsystem_threads() {
    const HELD_BYTES: usize = 4096;
    const HARNESS_BYTES: usize = 8192;
    const SUBSYSTEM_BYTES: usize = 2048;

    let _warmup = measure_worker_allocations(0, 0);
    let harnessed = measure_worker_allocations(HARNESS_BYTES, SUBSYSTEM_BYTES);
    let counted = measure_worker_allocations(0, SUBSYSTEM_BYTES);
    let control = measure_worker_allocations(0, 0);

    assert_eq!(harnessed.held.len(), HELD_BYTES);
    assert_eq!(harnessed.subsystem.len(), SUBSYSTEM_BYTES);
    assert_eq!(harnessed.heap.held, counted.heap.held);
    assert_eq!(harnessed.live, counted.live);
    assert_eq!(counted.heap.held, control.heap.held + SUBSYSTEM_BYTES);
    assert_eq!(counted.live, control.live + SUBSYSTEM_BYTES);
    assert!(harnessed.heap.peak >= harnessed.heap.held);
}

#[test]
fn worker_allocations_remain_visible_after_the_shared_owner_drops() {
    const LEAK_BYTES: usize = 8192;

    let start_worker = AtomicBool::new(false);
    let worker_allocated = AtomicBool::new(false);
    let finish_worker = AtomicBool::new(false);
    let worker_released = AtomicBool::new(false);
    let allow_worker_exit = AtomicBool::new(false);
    let (send, receive) = mpsc::channel();

    let (before, with_owner, after_drop, after_release) = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            while !start_worker.load(Ordering::Acquire) {
                spin_loop();
            }
            let retained = Arc::new(black_box(vec![0_u8; LEAK_BYTES]));
            send.send(Arc::clone(&retained))
                .expect("the shared-owner receiver is alive");
            worker_allocated.store(true, Ordering::Release);
            while !finish_worker.load(Ordering::Acquire) {
                spin_loop();
            }
            drop(retained);
            worker_released.store(true, Ordering::Release);
            while !allow_worker_exit.load(Ordering::Acquire) {
                spin_loop();
            }
        });

        let before = memory::live_bytes();
        start_worker.store(true, Ordering::Release);
        while !worker_allocated.load(Ordering::Acquire) {
            spin_loop();
        }
        let owner = receive
            .try_recv()
            .expect("the worker sent its shared owner");
        let with_owner = memory::live_bytes();
        drop(owner);
        let after_drop = memory::live_bytes();
        finish_worker.store(true, Ordering::Release);
        while !worker_released.load(Ordering::Acquire) {
            spin_loop();
        }
        let after_release = memory::live_bytes();
        allow_worker_exit.store(true, Ordering::Release);
        worker.join().expect("the retaining worker must finish");
        (before, with_owner, after_drop, after_release)
    });

    assert!(with_owner >= before + LEAK_BYTES);
    assert_eq!(after_drop, with_owner);
    assert!(after_release + LEAK_BYTES <= after_drop);
}
