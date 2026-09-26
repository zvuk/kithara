use std::num::NonZeroU32;

use kithara_events::TrackId;
use kithara_signal::{SessionEpoch, SessionFrame, SourceSpan};
use kithara_sync::{
    ArmPermit, LoadGeneration, SyncApplied, SyncExecutionReject, SyncExecutionStamp,
    SyncGateBinding, SyncReceipt,
};
use kithara_warp::WarpMapRevision;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

use crate::rt::track::{PlayerResource, PlayerTrack, SyncFadeTail};

/// PCM already consumed from the staged reader off the audio callback.
pub(crate) struct PreparedFirst {
    pub(crate) stereo: [f32; 2],
    pub(crate) source: SourceSpan,
}

/// Exact installed lane sent from the executor into one player callback.
pub(crate) struct SyncTicket {
    pub(crate) item_id: TrackId,
    pub(crate) load: LoadGeneration,
    pub(crate) resource: Box<PlayerResource>,
    pub(crate) first: PreparedFirst,
    pub(crate) permit: ArmPermit,
    pub(crate) gate: SyncGateBinding,
    pub(crate) activation: SessionFrame,
    pub(crate) source_start: u64,
    pub(crate) epoch: SessionEpoch,
    pub(crate) output_rate: NonZeroU32,
    pub(crate) map: WarpMapRevision,
}

/// Audio-owned objects returned to the control thread without RT destruction.
pub(crate) enum SyncReturn {
    Ticket(SyncTicket),
    Track(PlayerTrack),
    Tail(SyncFadeTail),
}

/// The sole audio-thread writer for one allocated slot's execution receipts.
pub struct SyncReceiptTx(HeapProd<SyncReceipt>);

/// The Host-owned reader for one allocated slot's execution receipts.
pub struct SyncReceiptRx(HeapCons<SyncReceipt>);

/// Two slots held for one activation until its first PCM span is consumed.
#[must_use]
pub(crate) struct ReceiptReservation<'a> {
    tx: &'a mut SyncReceiptTx,
    armed: SyncReceipt,
    presented: SyncReceipt,
}

/// Make the per-slot, single-producer receipt channel.
#[must_use]
pub fn sync_receipts() -> (SyncReceiptTx, SyncReceiptRx) {
    let (tx, rx) = HeapRb::<SyncReceipt>::new(2).split();
    (SyncReceiptTx(tx), SyncReceiptRx(rx))
}

impl SyncReceiptTx {
    /// Reserve both receipts before the audio gate can be claimed.
    pub(crate) fn reserve_pair(
        &mut self,
        stamp: SyncExecutionStamp,
        applied: SyncApplied,
    ) -> Option<ReceiptReservation<'_>> {
        (self.0.vacant_len() >= 2).then_some(ReceiptReservation {
            tx: self,
            armed: SyncReceipt::Armed(stamp),
            presented: SyncReceipt::Presented(applied),
        })
    }

    /// Record a pre-claim rejection when its one bounded slot is available.
    pub(crate) fn publish_rejected(
        &mut self,
        stamp: SyncExecutionStamp,
        reason: SyncExecutionReject,
    ) -> bool {
        self.0
            .try_push(SyncReceipt::Rejected { stamp, reason })
            .is_ok()
    }
}

impl ReceiptReservation<'_> {
    /// Publish the already-constructed pair after actual nonempty PCM consumption.
    ///
    /// No other producer can take these slots before this reservation is spent.
    ///
    /// # Panics
    /// Panics if the reserved pair no longer fits in the sole producer's ring.
    pub(crate) fn publish(self) {
        let written = self
            .tx
            .0
            .push_iter([self.armed, self.presented].into_iter());
        assert_eq!(written, 2, "reserved sync receipts must fit");
    }
}

impl SyncReceiptRx {
    delegate::delegate! {
        to self.0 {
            /// Read one callback receipt on the Host owner thread.
            pub fn try_pop(&mut self) -> Option<SyncReceipt>;
            /// True once the sole audio-thread producer has been dropped.
            ///
            /// Host uses this after graph removal to prove the callback no longer
            /// owns this slot before retiring its permit cell.
            #[expr(!$)]
            #[call(write_is_held)]
            pub fn producer_gone(&self) -> bool;
        }
    }
}
