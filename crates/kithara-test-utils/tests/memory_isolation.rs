#![forbid(unsafe_code)]

use std::{
    hint::{black_box, spin_loop},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use kithara_test_utils::memory::{self, Counting};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn measure_excludes_allocations_from_other_threads() {
    const HELD_BYTES: usize = 4096;
    const FOREIGN_BYTES: usize = 8192;

    let start = AtomicBool::new(false);
    let foreign_allocated = AtomicBool::new(false);
    let finish_foreign = AtomicBool::new(false);

    let (before, held, observation, during) = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            while !start.load(Ordering::Acquire) {
                spin_loop();
            }
            let foreign = black_box(vec![0_u8; FOREIGN_BYTES]);
            foreign_allocated.store(true, Ordering::Release);
            while !finish_foreign.load(Ordering::Acquire) {
                spin_loop();
            }
            drop(foreign);
        });

        let before = memory::live_bytes();
        let (held, observation) = memory::measure(|| {
            let held = black_box(vec![0_u8; HELD_BYTES]);
            start.store(true, Ordering::Release);
            while !foreign_allocated.load(Ordering::Acquire) {
                spin_loop();
            }
            held
        });
        let during = memory::live_bytes();
        finish_foreign.store(true, Ordering::Release);
        worker.join().expect("the allocating worker must finish");
        (before, held, observation, during)
    });

    assert_eq!(held.len(), HELD_BYTES);
    assert_eq!(observation.held, HELD_BYTES);
    assert_eq!(observation.peak, HELD_BYTES);
    assert_eq!(during, before + HELD_BYTES);
}
