use kithara_platform::sync::{WaitGate, Weak};
use kithara_signal::SegmentId;
use kithara_test_utils::{hang::HangDetector, kithara};
use ringbuf::traits::{Consumer, Observer};

use super::{PcmPacket, PcmReceiver};

/// Wait for a packet or producer closure only when offline blocking is enabled.
#[kithara::flash(true)]
#[kithara::hang_watchdog]
pub(crate) fn wait_for_packet(receiver: &PcmReceiver) {
    wait_until(receiver, &mut __hang_detector, || {
        receiver.forward.try_peek().is_some()
    });
}

impl PcmReceiver {
    /// Supply the requested PCM up to the producer lead. A full lead can
    /// leave one ring slot available for a split quantum.
    #[kithara::flash(true)]
    #[kithara::hang_watchdog]
    pub(crate) fn prepare_cut_window(&self, segment: SegmentId, window: usize) {
        wait_until(self, &mut __hang_detector, || {
            let mut frames = 0;
            for (index, packet) in self.packets().enumerate() {
                let PcmPacket::Chunk(chunk) = packet else {
                    return true;
                };
                if chunk.meta.segment != segment
                    || chunk.meta.end_of_track
                    || index + 2 >= self.forward.capacity().get()
                {
                    return true;
                }
                frames += chunk.frames();
                if frames >= window {
                    return true;
                }
            }
            false
        });
    }
}
fn wait_until(receiver: &PcmReceiver, watchdog: &mut HangDetector, available: impl Fn() -> bool) {
    let Some(ready) = receiver.ready.as_ref().and_then(Weak::upgrade) else {
        return;
    };
    loop {
        let since = ready.current();
        let expired = watchdog.remaining().is_zero();
        if available() || !receiver.forward.write_is_held() {
            watchdog.reset();
            return;
        }
        if expired {
            watchdog.tick();
        }
        receiver.wake.wake();
        let remaining = watchdog.remaining();
        kithara::probe_event!(pcm_wait_started);
        ready.wait_timeout(since, remaining);
        kithara::probe_event!(pcm_wait_finished);
    }
}
