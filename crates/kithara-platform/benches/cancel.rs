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
    hint::black_box,
    pin::pin,
    task::{Context, Waker},
};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
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
        b.iter(|| black_box(CancelToken::root()));
    });
}

fn bench_derive_child(c: &mut Criterion) {
    let parent = CancelToken::root();
    c.bench_function("cancel/derive_child", |b| {
        b.iter(|| black_box(parent.child()));
    });
}

/// Headline: the RT read path. Flat in depth (one `Acquire` load).
fn bench_is_cancelled(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/is_cancelled");
    for depth in DEPTHS {
        let tok = chain(depth);
        g.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, _| {
            b.iter(|| black_box(tok.is_cancelled()));
        });
    }
    g.finish();
}

/// Register + unregister a sync cancel-waker on a node at `depth`.
fn bench_on_cancel_register(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/on_cancel_register");
    for depth in DEPTHS {
        let tok = chain(depth);
        g.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, _| {
            b.iter(|| {
                let guard = tok.on_cancel(|| {});
                black_box(&guard);
            });
        });
    }
    g.finish();
}

/// A master `cancel()` propagating to `n` descendants. Fresh tree per sample
/// (cancel is one-shot), so only the cancel call itself is timed.
fn bench_cancel_propagate(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/cancel_propagate");
    for n in WIDTHS {
        g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || wide(n),
                |tree| {
                    tree.0.cancel();
                    black_box(tree)
                },
                BatchSize::SmallInput,
            );
        });
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
        g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| black_box(grp.is_cancelled()));
        });
    }
    g.finish();
}

/// `CancelGroup::cancelled` churn: poll the group future once (parks one slot
/// on every source), then drop it (unregisters all slots).
fn bench_group_cancelled_poll(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/group_cancelled_poll");
    for n in WIDTHS {
        let (_sources, grp) = group(n);
        g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let mut cx = Context::from_waker(Waker::noop());
                let mut fut = pin!(grp.cancelled());
                fut.as_mut().poll(&mut cx)
            });
        });
    }
    g.finish();
}

/// Build a `CancelGroup` from `n` sources (the `Vec -> Arc<[_]>` cost).
fn bench_group_new(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel/group_new");
    for n in WIDTHS {
        let sources: Vec<CancelToken> = (0..n).map(|_| CancelToken::never()).collect();
        g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || sources.clone(),
                |v| black_box(CancelGroup::new(v)),
                BatchSize::SmallInput,
            );
        });
    }
    g.finish();
}

criterion_group!(
    benches,
    bench_create_root,
    bench_derive_child,
    bench_is_cancelled,
    bench_on_cancel_register,
    bench_cancel_propagate,
    bench_cancelled_poll_churn,
    bench_group_is_cancelled,
    bench_group_cancelled_poll,
    bench_group_new,
);
criterion_main!(benches);
