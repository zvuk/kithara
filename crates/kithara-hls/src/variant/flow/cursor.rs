use std::sync::atomic::Ordering;

use kithara_test_utils::kithara;

use super::HlsVariant;

impl HlsVariant {
    #[kithara::probe(variant = self.variant as u64, n)]
    pub(crate) fn advance(&self, n: u64) {
        self.flow.reader.advance(n);
        if !self.flow.reader.is_seek_active() {
            self.clear_seek_alias_if_moved(self.flow.reader.position());
        }
    }

    #[kithara::probe(variant = self.variant as u64, pos = self.flow.reader.position())]
    pub(crate) fn get_position(&self) -> u64 {
        self.flow.reader.position()
    }

    pub(crate) fn prefetch_anchor(&self) -> u64 {
        self.flow.prefetch_anchor.load(Ordering::Acquire)
    }

    /// Record the cursor byte at which the front-of-queue segment enters the
    /// look-ahead window, or `u64::MAX` when nothing is deferred. Written by
    /// [`HlsVariant::dispatch`] on every pass, so it always describes the
    /// decision the peer last took.
    pub(super) fn defer_prefetch_until(&self, byte: u64) {
        self.flow.prefetch_resume_at.store(byte, Ordering::Release);
    }

    /// Whether the reader just made the deferred dispatch decision stale.
    /// Consumes the threshold, so it answers `true` exactly once per deferred
    /// segment: the next `dispatch` publishes the threshold for the segment
    /// after it. This is the reader's own progress re-opening a decision that
    /// was taken against an older cursor — no timer re-checks it.
    pub(crate) fn take_prefetch_resume(&self) -> bool {
        let at = self.flow.prefetch_resume_at.load(Ordering::Acquire);
        at != u64::MAX
            && self.flow.reader.position() >= at
            && self
                .flow
                .prefetch_resume_at
                .swap(u64::MAX, Ordering::AcqRel)
                != u64::MAX
    }

    #[kithara::probe(variant = self.variant as u64, pos)]
    pub(crate) fn set_position(&self, pos: u64) {
        let moved = self.flow.reader.position() != pos;
        self.set_position_without_byte_demand(pos);
        if moved {
            self.set_exact_byte_seek_demand(pos);
        }
    }

    pub(super) fn set_position_without_byte_demand(&self, pos: u64) {
        if !self.flow.reader.is_seek_active() {
            self.clear_seek_alias_if_moved(pos);
        }
        self.flow.reader.set_position(pos);
    }

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub(crate) fn set_prefetch_anchor(&self, byte: u64) {
        self.flow.prefetch_anchor.store(byte, Ordering::Release);
    }
}
