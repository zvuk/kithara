//! Head-to-head micro-benchmarks: the legacy `CancellationToken` (walk-up
//! `rt_cancel`, `tokio_util` + `parking_lot`) vs the W3 `CancelRoot` /
//! `CancelToken` (propagate-down `common::cancel`, std-only). Both
//! implementations coexist in the tree at this commit, so this captures the
//! comparison before the legacy roots are deleted in W3 Task 3.4.
//!
//! The architectural trade-off the numbers expose:
//! - `is_cancelled()` — legacy walks `self -> root` and Acquire-loads each flag
//!   (O(depth)); the new token is a single Acquire load (O(1)). This is the
//!   real-time audio produce-core read path, run hot. **The headline.**
//! - `on_cancel()` — legacy registers the waker on every ancestor up the chain
//!   (O(depth) register + O(depth) unregister on drop); the new token registers
//!   on this node only (O(1)), reached by the cancel walk from above.
//! - `cancel()` — both propagate (legacy through the `tokio_util` inner tree +
//!   its own flag store; new by walking the `Weak` children down once). This
//!   compares the constant factors of the two propagation mechanisms.
//! - `cancelled()` poll churn — the `select!` cancel-safety path: register a
//!   waiter then drop it. Legacy uses the `tokio_util` waiter list; the new
//!   token uses a std `Node` waker slot. Polled with a noop waker, no runtime.
//! - creation / `child` — allocation cost: legacy mints a `tokio_util` token +
//!   an `Arc<ChainNode>` (`parking_lot::Mutex`); the new token an `Arc<Node>`.

use std::{
    future::Future,
    hint::black_box,
    pin::pin,
    task::{Context, Waker},
};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use kithara_platform::{CancelRoot, CancelToken, CancellationToken};

/// `is_cancelled()` walk lengths / `on_cancel` registration depths.
const DEPTHS: [usize; 4] = [1, 4, 16, 64];
/// Descendant counts a master cancel propagates to.
const WIDTHS: [usize; 3] = [1, 64, 512];

/// A legacy leaf `depth` `child_token()` derivations below a fresh root. The
/// returned leaf keeps the whole ancestor chain alive through its `Arc` parents.
fn legacy_chain(depth: usize) -> CancellationToken {
    let mut tok = CancellationToken::default();
    for _ in 0..depth {
        tok = tok.child_token();
    }
    tok
}

/// A new leaf `depth` `child()` derivations below a fresh root. The root handle
/// is dropped here; the chain stays alive via the node parent-liveness `Arc`s.
fn new_chain(depth: usize) -> CancelToken {
    let root = CancelRoot::default();
    let mut tok = root.child();
    for _ in 1..depth {
        tok = tok.child();
    }
    tok
}

/// A legacy root with `n` direct children kept alive (master + fan-out).
fn legacy_wide(n: usize) -> (CancellationToken, Vec<CancellationToken>) {
    let root = CancellationToken::default();
    let kids = (0..n).map(|_| root.child_token()).collect();
    (root, kids)
}

/// A new root with `n` direct children kept alive (master + fan-out).
fn new_wide(n: usize) -> (CancelRoot, Vec<CancelToken>) {
    let root = CancelRoot::default();
    let kids = (0..n).map(|_| root.child()).collect();
    (root, kids)
}

fn bench_create_root(c: &mut Criterion) {
    let mut group = c.benchmark_group("cancel/create_root");
    group.bench_function("legacy", |b| {
        b.iter(|| black_box(CancellationToken::default()));
    });
    group.bench_function("new", |b| {
        b.iter(|| black_box(CancelRoot::default()));
    });
    group.finish();
}

fn bench_derive_child(c: &mut Criterion) {
    let mut group = c.benchmark_group("cancel/derive_child");
    let legacy_parent = CancellationToken::default();
    group.bench_function("legacy", |b| {
        b.iter(|| black_box(legacy_parent.child_token()));
    });
    let new_parent = CancelRoot::default();
    group.bench_function("new", |b| {
        b.iter(|| black_box(new_parent.child()));
    });
    group.finish();
}

/// Headline: the RT read path. Legacy scales with depth, the new token is flat.
fn bench_is_cancelled(c: &mut Criterion) {
    let mut group = c.benchmark_group("cancel/is_cancelled");
    for depth in DEPTHS {
        let legacy = legacy_chain(depth);
        group.bench_with_input(BenchmarkId::new("legacy", depth), &depth, |b, _| {
            b.iter(|| black_box(legacy.is_cancelled()));
        });
        let new = new_chain(depth);
        group.bench_with_input(BenchmarkId::new("new", depth), &depth, |b, _| {
            b.iter(|| black_box(new.is_cancelled()));
        });
    }
    group.finish();
}

/// Register + unregister a sync cancel-waker. Legacy walks every ancestor; the
/// new token touches one node.
fn bench_on_cancel_register(c: &mut Criterion) {
    let mut group = c.benchmark_group("cancel/on_cancel_register");
    for depth in DEPTHS {
        let legacy = legacy_chain(depth);
        group.bench_with_input(BenchmarkId::new("legacy", depth), &depth, |b, _| {
            b.iter(|| {
                let guard = legacy.on_cancel(|| {});
                black_box(&guard);
            });
        });
        let new = new_chain(depth);
        group.bench_with_input(BenchmarkId::new("new", depth), &depth, |b, _| {
            b.iter(|| {
                let guard = new.on_cancel(|| {});
                black_box(&guard);
            });
        });
    }
    group.finish();
}

/// A master `cancel()` propagating to `n` descendants. Fresh tree per sample
/// (cancel is one-shot), so only the cancel call itself is timed.
fn bench_cancel_propagate(c: &mut Criterion) {
    let mut group = c.benchmark_group("cancel/cancel_propagate");
    for n in WIDTHS {
        group.bench_with_input(BenchmarkId::new("legacy", n), &n, |b, &n| {
            b.iter_batched(
                || legacy_wide(n),
                |tree| {
                    tree.0.cancel();
                    black_box(tree)
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_with_input(BenchmarkId::new("new", n), &n, |b, &n| {
            b.iter_batched(
                || new_wide(n),
                |tree| {
                    tree.0.cancel();
                    black_box(tree)
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// `select!` cancel-safety path: build a `cancelled()` future, poll it once to
/// register a waiter, then drop it to unregister. Polled with a noop waker — no
/// async runtime needed (the token is uncancelled, so the poll is `Pending`).
fn bench_cancelled_poll_churn(c: &mut Criterion) {
    let mut group = c.benchmark_group("cancel/cancelled_poll_churn");

    let legacy = CancellationToken::default();
    group.bench_function("legacy", |b| {
        b.iter(|| {
            let mut cx = Context::from_waker(Waker::noop());
            let mut fut = pin!(legacy.cancelled());
            // Returned to the harness so the poll (slot register) is not elided;
            // `fut` drops at the end of the closure, unregistering the slot.
            fut.as_mut().poll(&mut cx)
        });
    });

    let new = CancelToken::never();
    group.bench_function("new", |b| {
        b.iter(|| {
            let mut cx = Context::from_waker(Waker::noop());
            let mut fut = pin!(new.cancelled());
            fut.as_mut().poll(&mut cx)
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_create_root,
    bench_derive_child,
    bench_is_cancelled,
    bench_on_cancel_register,
    bench_cancel_propagate,
    bench_cancelled_poll_churn,
);
criterion_main!(benches);
