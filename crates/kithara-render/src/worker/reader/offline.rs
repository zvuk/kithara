use kithara_platform::sync::{WaitGate, Weak};
use kithara_test_utils::kithara;
use ringbuf::traits::{Consumer, Observer};

use super::PcmReceiver;

/// Wait for a packet or producer closure only when offline blocking is enabled.
#[kithara::flash(true)]
#[kithara::measure(label = "render.pcm.wait")]
#[kithara::hang_watchdog]
pub(crate) fn wait_for_packet(receiver: &PcmReceiver) {
    let Some(ready) = receiver.ready.as_ref().and_then(Weak::upgrade) else {
        return;
    };
    loop {
        let since = ready.current();
        let expired = __hang_detector.remaining().is_zero();
        if receiver.forward.try_peek().is_some() || !receiver.forward.write_is_held() {
            hang_reset!();
            return;
        }
        if expired {
            hang_tick!();
        }
        receiver.wake.wake();
        let _real_io = kithara_platform::flash::real_io();
        let remaining = __hang_detector.remaining();
        kithara::probe_event!(pcm_wait_started);
        ready.wait_timeout(since, remaining);
        kithara::probe_event!(pcm_wait_finished);
    }
}
