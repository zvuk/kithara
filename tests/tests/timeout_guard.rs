//! Verifies that `#[kithara::test(timeout(...))]` properly kills hanging tests
//! instead of blocking forever.
//!
//! Each test simulates a hang (infinite loop / spawn_blocking that never returns)
//! and expects the timeout mechanism to panic with "timed out".

use kithara_platform::time::Duration;

// ── Sync: infinite loop in a sync test ──────────────────────────────

#[kithara::test(timeout(Duration::from_secs(2)))]
#[should_panic(expected = "timed out")]
fn sync_infinite_loop_is_killed_by_timeout() {
    loop {
        std::hint::spin_loop();
    }
}

// ── Async: infinite loop in an async test ───────────────────────────

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
#[should_panic(expected = "timed out")]
async fn async_infinite_loop_is_killed_by_timeout() {
    loop {
        kithara_platform::yield_now().await;
    }
}

// ── Async: spawn_blocking that never returns ────────────────────────
//
// This is the exact pattern that caused the original hang:
// tokio::time::timeout fires, but Runtime::drop waits for the blocking
// thread forever.  The manual-runtime approach with shutdown_timeout
// prevents this.

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
#[should_panic(expected = "timed out")]
async fn async_spawn_blocking_zombie_is_killed_by_timeout() {
    let _handle = tokio::task::spawn_blocking(|| {
        loop {
            std::hint::spin_loop();
        }
    })
    .await;
}
