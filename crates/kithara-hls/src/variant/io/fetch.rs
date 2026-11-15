use kithara_assets::{AcquisitionResult, ReadSide, ResourceAcquisition, WriteSide};
use kithara_bufpool::HasPool;
use kithara_download::{DemandFn, FetchCmd, OnCompleteFn, OnSlowFn, WriterFn};
use kithara_platform::{CancelToken, sync::Arc};
use kithara_storage::ResourceStatus;
use url::Url;

use crate::{
    segment::{Downloading, FetchClaim, FetchSlot},
    signal::SizeSignal,
    variant::HlsVariant,
};

impl<S> HlsVariant<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Builds a fetch command whose completion settles the claim under its cancellation epoch.
    ///
    /// A concurrent cache commit supplies the authoritative on-disk length. The writer fires
    /// byte-arrival wakes before terminal settle, since stalled-escape reconciliation and the audio
    /// worker have no reader progress to notice otherwise.
    pub(in crate::variant) fn build_cmd(
        self: &Arc<Self>,
        url: Url,
        acq: ResourceAcquisition<S>,
        handle: FetchClaim<Downloading, S>,
        signal: SizeSignal,
        cancel: CancelToken,
    ) -> Option<FetchCmd> {
        let writer = match acq {
            AcquisitionResult::Pending(writer) => writer,
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
        let slot = FetchSlot {
            handle,
            raw: writer.raw_write_handle(),
            writer,
            cancel: cancel.clone(),
            signal: signal.clone(),
            bus: self.profile.bus.clone(),
        };
        let slow_slot = slot.handle.slot_state();
        let slow_signal = signal.clone();
        let on_slow: OnSlowFn = Box::new(move || {
            slow_slot.mark_slow();
            slow_signal.wake_peer();
        });
        let demand_slot = slot.handle.slot_state();
        let demand: DemandFn = Box::new(move || demand_slot.is_reader_demanded());
        let mut inner_writer = slot.writer();
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
                .maybe_headers(self.config.headers.clone())
                .writer(writer_fn)
                .on_slow(on_slow)
                .demand(demand)
                .on_complete(OnCompleteFn::from(slot))
                .build(),
        )
    }
}
