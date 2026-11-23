use kithara_platform::sync::{WaitGate, Weak};
use kithara_signal::SegmentId;
use kithara_test_utils::kithara;
use ringbuf::traits::{Consumer, Observer};

use super::{PcmPacket, PcmReceiver};

/// Wait for a packet or producer closure only when offline blocking is enabled.
pub(crate) fn wait_for_packet(receiver: &PcmReceiver) {
    wait_until(receiver, || receiver.forward.try_peek().is_some());
}

impl PcmReceiver {
    /// Supply the requested PCM up to the producer lead. A full lead can
    /// leave one ring slot available for a split quantum.
    pub(crate) fn prepare_cut_window(&self, segment: SegmentId, window: usize) {
        wait_until(self, || {
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

#[kithara::flash(true)]
#[kithara::measure(label = "render.pcm.wait")]
#[kithara::hang_watchdog]
fn wait_until(receiver: &PcmReceiver, available: impl Fn() -> bool) {
    let Some(ready) = receiver.ready.as_ref().and_then(Weak::upgrade) else {
        return;
    };
    loop {
        let since = ready.current();
        let expired = __hang_detector.remaining().is_zero();
        if available() || !receiver.forward.write_is_held() {
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
