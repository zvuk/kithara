//! Public-surface tests for the W3 propagate-down cancel types
//! (`CancelToken` / `CancelScope`). The OR-combinator
//! `CancelGroup` is not yet exported from the crate root (its root re-export
//! switches off the legacy in 3.3), so its tests live in `src/common/cancel/
//! group.rs` `#[cfg(test)]`.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use kithara_platform::{CancelToken, sync::Arc};
use kithara_test_utils::kithara;

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn select_cancel_branch_is_cancel_safe() {
    // The cancelled() branch losing the select! race must unregister its slot,
    // so a later poll of a fresh Cancelled does not leak or lose the wake.
    let master = CancelToken::root();
    let token = master.child();

    for _ in 0..3 {
        tokio::select! {
            () = token.cancelled() => unreachable!("not cancelled yet"),
            () = tokio::task::yield_now() => {}
        }
    }

    master.cancel();
    tokio::time::timeout(Duration::from_secs(2), token.cancelled())
        .await
        .expect("cancelled() must resolve after the master cancel");
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn recreate_cancelled_in_loop_does_not_leak_slots() {
    // Re-creating and dropping Cancelled many times must not accumulate waker
    // slots: a final cancel still wakes a fresh future. This drives the real
    // `Cancelled::drop` -> `unregister` wiring end-to-end; the slot-count BOUND
    // itself is pinned at the node layer
    // (`node::tests::register_unregister_churn_leaves_no_slots`), the only place
    // the private `slots` len is observable.
    let master = CancelToken::root();
    let token = master.child();

    for _ in 0..1000 {
        let mut fut = Box::pin(token.cancelled());
        // Poll once to register a slot, then drop to unregister it.
        let _ = futures::poll!(fut.as_mut());
    }

    master.cancel();
    tokio::time::timeout(Duration::from_secs(2), token.cancelled())
        .await
        .expect("cancelled() must still resolve after slot churn");
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn late_sync_registration_after_fire_fires_immediately() {
    let token = CancelToken::never();
    token.cancel();

    let n = Arc::new(AtomicUsize::new(0));
    let n2 = Arc::clone(&n);
    let _guard = token.on_cancel(move || {
        n2.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(
        n.load(Ordering::SeqCst),
        1,
        "a sync waker registered after the node fired must fire at once"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn late_task_registration_after_fire_resolves() {
    // A cancelled() future first polled after the node has already cancelled
    // must resolve, not park forever (the fired latch on the task path).
    let token = CancelToken::never();
    token.cancel();
    tokio::time::timeout(Duration::from_secs(2), token.cancelled())
        .await
        .expect("cancelled() first polled post-cancel must resolve");
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn flag_visible_after_cancelled_resolves() {
    // Release/Acquire ordering: a task that observed the cancel wake must see
    // the flag set (the Node sets the flag before firing wakers).
    let master = CancelToken::root();
    let token = master.child();
    let t = token.clone();
    let handle = tokio::spawn(async move {
        t.cancelled().await;
        t.is_cancelled()
    });
    tokio::task::yield_now().await;

    master.cancel();

    let seen = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("cancelled() must resolve")
        .expect("task must not panic");
    assert!(seen, "flag must be visible once the wake was observed");
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn late_child_is_born_cancelled() {
    let master = CancelToken::root();
    master.cancel();
    let child = master.child();
    assert!(
        child.is_cancelled(),
        "a child derived after the parent cancelled is born cancelled"
    );
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn repeat_cancel_is_idempotent() {
    let token = CancelToken::never();
    token.cancel();
    token.cancel();
    assert!(token.is_cancelled());
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn drop_intermediate_token_root_cancel_reaches_grandchild() {
    // PARENT-LIVENESS PIN: drop the only handle to the intermediate token; the
    // grandchild must still observe a root cancel.
    let master = CancelToken::root();
    let leaf = {
        let mid = master.child();
        mid.child()
    };
    assert!(!leaf.is_cancelled());
    master.cancel();
    assert!(
        leaf.is_cancelled(),
        "root cancel must reach the grandchild after the intermediate token was dropped"
    );
}
