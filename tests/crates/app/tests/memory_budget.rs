//! What each subsystem of the desktop composition root holds on the heap.
//!
//! The application's own footprint is dominated by the GPU renderer and, in a
//! debug build, by its code and symbols — neither of which a test process can
//! stand in for. What this pins is the half a test *can* own: the non-GUI
//! composition root, built exactly the way `kithara-app`'s `main` builds it,
//! with a budget per subsystem so a regression names the subsystem that grew
//! rather than a single total that says only "more".

use kithara::{
    assets::StorageBackend,
    download::{Downloader, DownloaderConfig},
    net::{HttpClient, NetOptions},
    platform::CancelToken,
    play::{PlayWorker, PlayWorkerConfig},
};
use kithara_app::pools::{AppStore, AppWorker, Pools, PoolsSection, build};
use kithara_test_utils::memory;

/// Budgets in kibibytes, held after construction.
///
/// Every value is the measured cost plus room for allocator jitter, and each
/// one is charged to the subsystem that spent it. They are deliberately
/// separate numbers: a single total hides which half moved.
struct Budget;

impl Budget {
    /// The pool region preallocates `INITIAL_SAMPLE_BUFFERS` sample buffers,
    /// and is the only subsystem here that costs anything at construction.
    /// Measured at 603 KiB.
    const POOLS_KIB: usize = 1_024;
    /// The asset store on its in-memory backend, with no assets yet.
    /// Measured at 84 KiB.
    const STORE_KIB: usize = 256;
    /// One playback worker and the threads it owns. Measured at 3 KiB: a
    /// thread's stack is not heap, and the worker's buffers are the pool's.
    const WORKER_KIB: usize = 256;
    /// One HTTP client and the downloader in front of it. Measured at 15 KiB
    /// — the TLS stack is built on first use, not here.
    const DOWNLOADER_KIB: usize = 256;
    /// Everything above, standing at once. Measured at 707 KiB.
    const TOTAL_KIB: usize = 1_536;
}

fn pools() -> Pools {
    build(&PoolsSection::default()).unwrap_or_else(|error| panic!("app pool region: {error}"))
}

#[kithara::test(native, flash(false))]
fn each_subsystem_of_the_composition_root_stays_inside_its_budget() {
    let shutdown = CancelToken::root();

    let (pools, pool_heap) = memory::measure(pools);
    let (store, store_heap) = memory::measure(|| {
        AppStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .cancel(shutdown.child())
            .build()
    });
    let (worker, worker_heap) = memory::measure(|| {
        AppWorker::new(
            PlayWorkerConfig::builder(pools.clone())
                .cancel(shutdown.child())
                .build(),
        )
    });
    let (downloader, downloader_heap) = memory::measure(|| {
        let net = NetOptions::builder().build();
        let client = HttpClient::new(net, pools.clone(), shutdown.child());
        Downloader::new(DownloaderConfig::for_client(client).build())
    });

    for (subsystem, heap, budget_kib) in [
        ("pools", pool_heap, Budget::POOLS_KIB),
        ("store", store_heap, Budget::STORE_KIB),
        ("worker", worker_heap, Budget::WORKER_KIB),
        ("downloader", downloader_heap, Budget::DOWNLOADER_KIB),
    ] {
        let (held_kib, peak_kib) = heap.kib();
        eprintln!(
            "{subsystem}: holds {held_kib} KiB, peaked at {peak_kib} KiB (budget {budget_kib} KiB)"
        );
        assert!(
            held_kib <= budget_kib,
            "{subsystem} holds {held_kib} KiB, over its {budget_kib} KiB budget"
        );
    }

    let total_kib =
        (pool_heap.held + store_heap.held + worker_heap.held + downloader_heap.held) / 1024;
    eprintln!(
        "composition root: {total_kib} KiB (budget {} KiB)",
        Budget::TOTAL_KIB
    );
    assert!(
        total_kib <= Budget::TOTAL_KIB,
        "the composition root holds {total_kib} KiB, over its {} KiB budget",
        Budget::TOTAL_KIB
    );

    drop((store, worker, downloader, pools));
}

#[kithara::test(native, flash(false))]
fn a_rebuilt_composition_root_returns_to_its_baseline() {
    let shutdown = CancelToken::root();
    let baseline = memory::live_bytes();

    for _ in 0..4 {
        let pools = pools();
        let store = AppStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .cancel(shutdown.child())
            .build();
        let worker = PlayWorker::new(
            PlayWorkerConfig::builder(pools.clone())
                .cancel(shutdown.child())
                .build(),
        );
        drop((store, worker, pools));
    }

    let left = memory::live_bytes().saturating_sub(baseline);
    eprintln!("after four build/drop cycles: {left} B left");
    assert!(
        left <= Budget::TOTAL_KIB * 1024,
        "four build/drop cycles left {left} B on the heap"
    );
}
