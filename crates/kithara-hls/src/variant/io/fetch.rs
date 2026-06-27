use std::sync::Arc;

use kithara_assets::{AcquisitionResult, AssetResource, ReadSide, WriteSide};
use kithara_drm::DecryptContext;
use kithara_storage::ResourceStatus;
use kithara_stream::dl::{FetchCmd, OnCompleteFn, OnSlowFn, WriterFn};
use url::Url;

use super::HlsVariant;
use crate::{
    segment::{Downloading, FetchClaim, FetchSlot},
    signal::SizeSignal,
};

impl HlsVariant {
    /// Common assembly for init and segment fetches. Both go through the
    /// same `FetchSlot`: writer streams to the asset resource, `on_complete`
    /// runs `settle` which observes `cancel.is_cancelled()` as the epoch
    /// gate.
    pub(super) fn build_cmd(
        self: &Arc<Self>,
        url: Url,
        acq: AssetResource<DecryptContext>,
        handle: FetchClaim<Downloading>,
        signal: SizeSignal,
    ) -> Option<FetchCmd> {
        let writer = match acq {
            AcquisitionResult::Pending(writer) => writer,
            // Committed between the skip-fetch probe and acquire — no download.
            // Mirror `settle_success`: mark the segment loaded at its on-disk
            // length so announced/estimated sizes match the bytes on disk.
            AcquisitionResult::Ready(reader) => {
                match reader.status() {
                    ResourceStatus::Committed { final_len: Some(n) } => {
                        handle.into_loaded(n);
                    }
                    _ => {
                        handle.into_loaded_no_apply();
                    }
                }
                signal.fire();
                return None;
            }
            _ => {
                handle.into_missing();
                return None;
            }
        };
        let cancel = self.cancel_handle();
        let slot = FetchSlot {
            handle,
            reader: writer.reader(),
            raw: writer.raw_write_handle(),
            writer,
            cancel: cancel.clone(),
            signal: signal.clone(),
        };
        // Capture the slot's CAS cell before the claim moves into `on_complete`
        // so the slow hook can flag this in-flight fetch when it crosses the
        // downloader's `soft_timeout`. The ABR stalled-escape gate reads it.
        let slow_slot = slot.handle.slot_state();
        let on_slow: OnSlowFn = Box::new(move || slow_slot.mark_slow());
        let mut inner_writer = slot.writer();
        // Per-chunk write signal: wake a reader parked in `wait_range(_, None)`
        // the moment bytes land (not only on commit), so a sub-segment range
        // resolves without waiting for settle. Also re-ticks the RT decoder's
        // audio worker (off the 10 ms scheduler poll) the instant plaintext
        // bytes land. Runs on the downloader thread (off-RT) — taking the
        // gate's condvar mutex and the wait-free worker unpark are allowed here.
        let writer_fn: WriterFn = Box::new(move |chunk: &[u8]| {
            let result = inner_writer(chunk);
            if result.is_ok() {
                signal.fire();
            }
            result
        });
        Some(
            FetchCmd::get(url)
                .cancel(cancel)
                .maybe_headers(self.profile.headers.clone())
                .writer(writer_fn)
                .on_slow(on_slow)
                .on_complete(OnCompleteFn::from(slot))
                .build(),
        )
    }
}
