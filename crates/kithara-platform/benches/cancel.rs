//! Micro-benchmarks for the propagate-down cancel primitives
//! (`common::cancel`): `CancelToken` and the `CancelGroup`
//! OR-combinator. Runtime-free — `cancelled()` futures are polled by hand with
//! a noop waker (uncancelled tokens stay `Pending`), so the numbers isolate the
//! primitive, not a scheduler.
//!
//! What each group measures:
//! - `is_cancelled` — the real-time audio produce-core read path: a single
//!   `Acquire` load of one flag, flat in tree depth. **The headline.**
//! - `on_cancel` — register + unregister one sync cancel-waker on a node.
//! - `cancel` — a master `cancel()` propagating DOWN to N descendants.
//! - `cancelled` poll churn — the `select!` cancel-safety path: register a
//!   waiter then drop it.
//! - creation / `child` — allocation cost (`Arc<Node>`).
//! - `group/*` — the `CancelGroup` fan-in: `is_cancelled` polls each source
//!   (N×O(1)); `cancelled` parks one slot per source in a single future.
use std::{
    future::Future,
    pin::pin,
    task::{Context, Waker},
};

use criterion::{BatchSize, BenchmarkId, Criterion};
use kithara_platform::{CancelGroup, CancelToken};

/// `is_cancelled()` walk lengths / `on_cancel` registration depths.
const DEPTHS: [usize; 4] = [1, 4, 16, 64];
/// Descendant / source counts a master cancel propagates to / a group fans in.
const WIDTHS: [usize; 3] = [1, 64, 512];

/// A leaf `depth` `child()` derivations below a fresh root. The root handle is
/// dropped here; the chain stays alive via the node parent-liveness `Arc`s.
fn chain(depth: usize) -> CancelToken {
    let root = CancelToken::root();
    let mut tok = root.child();
    for _ in 1..depth {
        tok = tok.child();
    }
    tok
}

/// A root with `n` direct children kept alive (master + fan-out).
fn wide(n: usize) -> (CancelToken, Vec<CancelToken>) {
    let root = CancelToken::root();
    let kids = (0..n).map(|_| root.child()).collect();
    (root, kids)
}

/// A group fanning in `n` fresh source tokens (returned alongside so they stay
/// alive for the duration of the bench).
fn group(n: usize) -> (Vec<CancelToken>, CancelGroup) {
    let sources: Vec<CancelToken> = (0..n).map(|_| CancelToken::never()).collect();
    let group = CancelGroup::new(sources.clone());
    (sources, group)
}

fn bench_create_root(c: &mut Criterion) {
    c.bench_function("cancel/create_root", |b| {
        b.iter(|| std::hint::black_box(CancelToken::root()));
    });
}

fn bench_derive_child(c: &mut Criterion) {
    let parent = CancelToken::root();
    c.bench_function("cancel/derive_child", |b| {
        b.iter(|| std::hint::black_box(parent.child()));
    });
}

/// Headline: the RT read path. Flat in depth (one `Acquire` load).
fn bench_is_cancelled(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/is_cancelled");
    for depth in DEPTHS {
        let tok = chain(depth);
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("depth_{depth}")),
            &depth,
            |b, _| {
                b.iter(|| std::hint::black_box(tok.is_cancelled()));
            },
        );
    }
    g.finish();
}

/// Register + unregister a sync cancel-waker on a node at `depth`.
fn bench_on_cancel_register(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/on_cancel_register");
    for depth in DEPTHS {
        let tok = chain(depth);
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("depth_{depth}")),
            &depth,
            |b, _| {
                b.iter(|| {
                    let guard = tok.on_cancel(|| {});
                    std::hint::black_box(&guard);
                });
            },
        );
    }
    g.finish();
}

/// A master `cancel()` propagating to `n` descendants. Fresh tree per sample
/// (cancel is one-shot), so only the cancel call itself is timed.
fn bench_cancel_propagate(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/cancel_propagate");
    for n in WIDTHS {
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("width_{n}")),
            &n,
            |b, &n| {
                b.iter_batched(
                    || wide(n),
                    |tree| {
                        tree.0.cancel();
                        std::hint::black_box(tree)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    g.finish();
}

/// `select!` cancel-safety path: build a `cancelled()` future, poll it once to
/// register a waiter, then drop it to unregister. Polled with a noop waker — no
/// async runtime (the token is uncancelled, so the poll is `Pending`).
fn bench_cancelled_poll_churn(c: &mut Criterion) {
    let token = CancelToken::never();
    c.bench_function("cancel/cancelled_poll_churn", |b| {
        b.iter(|| {
            let mut cx = Context::from_waker(Waker::noop());
            let mut fut = pin!(token.cancelled());
            fut.as_mut().poll(&mut cx)
        });
    });
}

/// `CancelGroup::is_cancelled` over `n` uncancelled sources (N×O(1) flag loads).
fn bench_group_is_cancelled(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/group_is_cancelled");
    for n in WIDTHS {
        let (_sources, grp) = group(n);
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("width_{n}")),
            &n,
            |b, _| {
                b.iter(|| std::hint::black_box(grp.is_cancelled()));
            },
        );
    }
    g.finish();
}

/// `CancelGroup::cancelled` churn: poll the group future once (parks one slot
/// on every source), then drop it (unregisters all slots).
fn bench_group_cancelled_poll(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/group_cancelled_poll");
    for n in WIDTHS {
        let (_sources, grp) = group(n);
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("width_{n}")),
            &n,
            |b, _| {
                b.iter(|| {
                    let mut cx = Context::from_waker(Waker::noop());
                    let mut fut = pin!(grp.cancelled());
                    fut.as_mut().poll(&mut cx)
                });
            },
        );
    }
    g.finish();
}

/// Build a `CancelGroup` from `n` sources (the `Vec -> Arc<[_]>` cost).
fn bench_group_new(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/group_new");
    for n in WIDTHS {
        let sources: Vec<CancelToken> = (0..n).map(|_| CancelToken::never()).collect();
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("width_{n}")),
            &n,
            |b, _| {
                b.iter_batched(
                    || sources.clone(),
                    |v| std::hint::black_box(CancelGroup::new(v)),
                    BatchSize::SmallInput,
                );
            },
        );
    }
    g.finish();
}

fn criterion_benchmarks(c: &mut Criterion) {
    bench_create_root(c);
    bench_derive_child(c);
    bench_is_cancelled(c);
    bench_on_cancel_register(c);
    bench_cancel_propagate(c);
    bench_cancelled_poll_churn(c);
    bench_group_is_cancelled(c);
    bench_group_cancelled_poll(c);
    bench_group_new(c);
}

#[metabench::benchmark(CANCEL_CREATE_ROOT, "cancel", "create_root")]
fn native_create_root() -> CancelToken {
    CancelToken::root()
}

fn parent() -> CancelToken {
    CancelToken::root()
}

#[metabench::benchmark(CANCEL_DERIVE_CHILD, "cancel", "derive_child", gungraun_setup = parent)]
fn native_derive_child(parent: CancelToken) -> (CancelToken, CancelToken) {
    let child = parent.child();
    (parent, child)
}

#[metabench::benchmark(CANCEL_IS_CANCELLED, "cancel", "is_cancelled", gungraun_setup = chain)]
#[bench::depth_1(1_usize)]
#[bench::depth_4(4_usize)]
#[bench::depth_16(16_usize)]
#[bench::depth_64(64_usize)]
fn native_is_cancelled(token: CancelToken) -> (CancelToken, bool) {
    let cancelled = token.is_cancelled();
    (token, cancelled)
}

#[metabench::benchmark(CANCEL_ON_CANCEL, "cancel", "on_cancel_register", gungraun_setup = chain)]
#[bench::depth_1(1_usize)]
#[bench::depth_4(4_usize)]
#[bench::depth_16(16_usize)]
#[bench::depth_64(64_usize)]
fn native_on_cancel(token: CancelToken) -> CancelToken {
    let guard = token.on_cancel(|| {});
    std::hint::black_box(&guard);
    drop(guard);
    token
}

#[metabench::benchmark(CANCEL_PROPAGATE, "cancel", "cancel_propagate", gungraun_setup = wide)]
#[bench::width_1(1_usize)]
#[bench::width_64(64_usize)]
#[bench::width_512(512_usize)]
fn native_propagate(tree: (CancelToken, Vec<CancelToken>)) -> (CancelToken, Vec<CancelToken>) {
    tree.0.cancel();
    tree
}

fn never() -> CancelToken {
    CancelToken::never()
}

#[metabench::benchmark(CANCEL_POLL_CHURN, "cancel", "cancelled_poll_churn", gungraun_setup = never)]
fn native_poll_churn(token: CancelToken) -> CancelToken {
    let mut cx = Context::from_waker(Waker::noop());
    {
        let mut fut = pin!(token.cancelled());
        let _ = std::hint::black_box(fut.as_mut().poll(&mut cx));
    }
    token
}

#[metabench::benchmark(CANCEL_GROUP_IS_CANCELLED, "cancel", "group_is_cancelled", gungraun_setup = group)]
#[bench::width_1(1_usize)]
#[bench::width_64(64_usize)]
#[bench::width_512(512_usize)]
fn native_group_is_cancelled(
    input: (Vec<CancelToken>, CancelGroup),
) -> (Vec<CancelToken>, CancelGroup, bool) {
    let cancelled = input.1.is_cancelled();
    (input.0, input.1, cancelled)
}

#[metabench::benchmark(CANCEL_GROUP_POLL, "cancel", "group_cancelled_poll", gungraun_setup = group)]
#[bench::width_1(1_usize)]
#[bench::width_64(64_usize)]
#[bench::width_512(512_usize)]
fn native_group_poll(input: (Vec<CancelToken>, CancelGroup)) -> (Vec<CancelToken>, CancelGroup) {
    let mut cx = Context::from_waker(Waker::noop());
    {
        let mut fut = pin!(input.1.cancelled());
        let _ = std::hint::black_box(fut.as_mut().poll(&mut cx));
    }
    input
}

fn sources(n: usize) -> Vec<CancelToken> {
    (0..n).map(|_| CancelToken::never()).collect()
}

#[metabench::benchmark(CANCEL_GROUP_NEW, "cancel", "group_new", gungraun_setup = sources)]
#[bench::width_1(1_usize)]
#[bench::width_64(64_usize)]
#[bench::width_512(512_usize)]
fn native_group_new(sources: Vec<CancelToken>) -> CancelGroup {
    CancelGroup::new(sources)
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [
        CANCEL_CREATE_ROOT,
        CANCEL_DERIVE_CHILD,
        CANCEL_IS_CANCELLED,
        CANCEL_ON_CANCEL,
        CANCEL_PROPAGATE,
        CANCEL_POLL_CHURN,
        CANCEL_GROUP_IS_CANCELLED,
        CANCEL_GROUP_POLL,
        CANCEL_GROUP_NEW,
    ],
    allocator = std::alloc::System,
);
