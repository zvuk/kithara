use std::{cell::Cell, future, panic::catch_unwind};

use kithara::platform::{
    no_block::{forbid_bridged, force_panic_mode},
    time::Duration,
    tokio::{runtime::Handle, task},
};
use kithara_integration_tests::kithara;

struct ScopedOwner {
    runtime: Handle,
    body_ran: Cell<bool>,
}

fn assert_outside_async_poll() {
    let _mode = force_panic_mode();
    assert!(
        catch_unwind(|| forbid_bridged(None)).is_ok(),
        "scoped fixture lifecycle entered an async poll"
    );
}

fn scoped_owner() -> ScopedOwner {
    assert_outside_async_poll();
    ScopedOwner {
        runtime: Handle::try_current().expect("fixture runtime is entered"),
        body_ran: Cell::new(false),
    }
}

impl Drop for ScopedOwner {
    fn drop(&mut self) {
        assert_outside_async_poll();
        assert!(self.body_ran.get(), "fixture outlived the test body");
        Handle::try_current().expect("fixture runtime remains entered through drop");
        self.runtime
            .block_on(task::spawn_blocking_on(&self.runtime, || ()))
            .expect("fixture drops before runtime shutdown");
    }
}

#[kithara::test(native, tokio)]
async fn borrowed_fixture_finishes_before_runtime_shutdown(#[by_ref] scoped_owner: &ScopedOwner) {
    scoped_owner.body_ran.set(true);
}

#[kithara::test(native, tokio)]
#[should_panic(expected = "[no_block]")]
async fn borrowing_a_fixture_keeps_the_body_watched(#[by_ref] scoped_owner: &ScopedOwner) {
    scoped_owner.body_ran.set(true);
    let _mode = force_panic_mode();
    forbid_bridged(None);
}

#[kithara::test(native, tokio)]
#[should_panic(expected = "scoped body panic")]
async fn borrowed_fixture_drops_outside_a_panicking_poll(#[by_ref] scoped_owner: &ScopedOwner) {
    scoped_owner.body_ran.set(true);
    panic!("scoped body panic");
}

#[kithara::test(native, tokio, flash(false), timeout(Duration::from_millis(25)))]
#[should_panic(expected = "timed out")]
async fn borrowed_fixture_drops_before_timeout_runtime_shutdown(
    #[by_ref] scoped_owner: &ScopedOwner,
) {
    scoped_owner.body_ran.set(true);
    future::pending::<()>().await;
}
