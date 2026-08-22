mod cancel;
mod reader;

use std::{
    io::{self, Error, ErrorKind},
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    ByteMap, ConstructionGate, PendingReason, ReaderProfile, SeekObserve, SegmentDescriptor,
    SourceError, SourcePhase, SourceSeekAnchor, StreamError, StreamResult, VariantTransition,
};

use self::cancel::SessionCancel;
pub(super) use self::reader::HlsSessionReader;
use super::coord::HlsCoord;
use crate::{
    signal::SizeSignal,
    variant::{HlsVariant, ResolvedSeekProjection, VariantReaderPreparation},
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct HlsSession {
    seek: Arc<dyn SeekObserve>,
    variant: Arc<HlsVariant>,
    active: AtomicBool,
    position: AtomicU64,
    construction_gate: ConstructionGate,
    #[field(get, vis = "pub(crate)", copy)]
    transition: Option<VariantTransition>,
    cancel: SessionCancel,
    readiness: SessionReadiness,
    signal: SizeSignal,
    #[field(get, vis = "pub(crate)")]
    variant_index: usize,
}

enum SessionReadiness {
    Active,
    Profiled {
        preparation: Mutex<VariantReaderPreparation>,
        profile: ReaderProfile,
    },
}

#[derive(Clone, Copy)]
struct SessionPosition {
    projection: Option<ResolvedSeekProjection>,
    byte: u64,
}

impl HlsSession {
    pub(crate) fn activate(&self) {
        self.active.store(true, Ordering::Release);
    }

    pub(crate) fn active(
        cancel: CancelToken,
        seek: Arc<dyn SeekObserve>,
        signal: SizeSignal,
        variant_index: usize,
        variant: Arc<HlsVariant>,
        position: u64,
    ) -> Self {
        Self {
            seek,
            signal,
            variant,
            variant_index,
            active: AtomicBool::new(true),
            cancel: SessionCancel::new(cancel),
            construction_gate: ConstructionGate::default(),
            position: AtomicU64::new(position),
            readiness: SessionReadiness::Active,
            transition: None,
        }
    }

    pub(crate) fn advance(&self, bytes: u64) {
        let position = self.projected_position();
        self.advance_from(position, bytes);
    }

    fn advance_from(&self, position: SessionPosition, bytes: u64) {
        let next = position.byte.wrapping_add(bytes);
        self.position.store(next, Ordering::Release);
        if bytes == 0 {
            return;
        }
        if let Some(projection) = position.projection {
            self.variant.retire_seek_projection(projection);
        } else {
            self.variant.retire_seek_projection_if_moved(next);
        }
    }

    delegate::delegate! {
        to self.cancel {
            pub(crate) fn abort(&self);
            /// Retire this session's look-ahead fetches — queued and in
            /// flight — while keeping the owed window live. Called when an
            /// incoming variant slot is installed: the capacity those fetches
            /// hold is what the construction needs, and their bytes lie past
            /// the cut the transition latches.
            pub(crate) fn retire_lookahead(&self);
        }
        to self.signal {
            pub(crate) fn arm_peer(&self);
            /// Ask the peer to plan for a session that answered [`Self::is_ready`] with
            /// `false`. Call it only with no transition lock held.
            #[call(wake_peer)]
            pub(crate) fn wake_peer_for_readiness(&self);
        }
        to self.construction_gate {
            #[call(is_armed)]
            pub(crate) fn construction_blocking(&self) -> bool;
            #[call(clone)]
            pub(crate) fn construction_gate(&self) -> ConstructionGate;
        }
        to self.variant {
            pub(crate) fn find_at_offset(&self, byte: u64) -> Option<(u32, u64, u64)>;
            #[call(stream_len)]
            pub(crate) fn len(&self) -> Option<u64>;
            pub(crate) fn media_info(&self) -> kithara_stream::MediaInfo;
        }
    }

    fn check_live(&self) -> io::Result<()> {
        if self.cancel.root.is_cancelled() {
            return Err(Error::other("HLS reader session cancelled"));
        }
        if !self.active.load(Ordering::Acquire)
            && self
                .transition
                .is_some_and(|transition| self.seek.epoch() != transition.id().seek_epoch())
        {
            return Err(pending(PendingReason::SeekPending));
        }
        Ok(())
    }

    /// Fetches for a session that owns a live reader.
    ///
    /// Bounded by that reader's own position and the peer's look-ahead, the
    /// same way the audible session is bounded.
    pub(crate) fn dispatch(
        &self,
        ctx: &crate::variant::PlanCtx,
        budget: usize,
    ) -> Vec<kithara_stream::dl::FetchCmd> {
        self.dispatch_capped(ctx, budget, None)
    }

    fn dispatch_capped(
        &self,
        ctx: &crate::variant::PlanCtx,
        budget: usize,
        construction_segment_end: Option<u32>,
    ) -> Vec<kithara_stream::dl::FetchCmd> {
        if self.cancel.is_cancelled() {
            return Vec::new();
        }
        self.variant.dispatch_from(
            ctx,
            budget,
            self.projected_position().byte,
            construction_segment_end,
            self.active.load(Ordering::Acquire),
            self.cancel.dispatch_tokens(),
        )
    }

    /// Fetches for the audible session while a variant transition is building.
    ///
    /// Capped at the owed window — the playing segment and the next — so the
    /// outgoing look-ahead cannot hold downloader capacity the incoming
    /// construction is waiting on. Everything past the latched cut is dead to
    /// the splice anyway; only the frontier the reader is consuming stays owed.
    pub(crate) fn dispatch_owed(
        &self,
        ctx: &crate::variant::PlanCtx,
        budget: usize,
    ) -> Vec<kithara_stream::dl::FetchCmd> {
        let owed_end = self
            .find_at_offset(self.projected_position().byte)
            .map(|(seg_idx, _, _)| seg_idx.saturating_add(1));
        self.dispatch_capped(ctx, budget, owed_end)
    }

    /// Fetches for an incoming session whose reader has not been handed to a
    /// decoder yet.
    ///
    /// Capped at the construction window, because until the transfer the
    /// session has no reader position to follow and would otherwise queue the
    /// whole variant against the audible one. The cap ends at the transfer:
    /// that window is sized to *build* a decoder, not to feed one, and a
    /// priming decoder that must stage seconds of audio starves behind it —
    /// its reads stop being served, the staged span stops growing, and the
    /// outgoing frontier it is chasing walks away for good.
    pub(crate) fn dispatch_constructing(
        &self,
        ctx: &crate::variant::PlanCtx,
        budget: usize,
    ) -> Vec<kithara_stream::dl::FetchCmd> {
        let cap = match &self.readiness {
            SessionReadiness::Active => None,
            SessionReadiness::Profiled { preparation, .. } => {
                Some(self.variant.construction_segment_end(&preparation.lock()))
            }
        };
        self.dispatch_capped(ctx, budget, cap)
    }

    pub(crate) fn incoming(
        cancel: CancelToken,
        profile: ReaderProfile,
        seek: Arc<dyn SeekObserve>,
        signal: SizeSignal,
        transition: VariantTransition,
        variant: Arc<HlsVariant>,
        content_time: Duration,
    ) -> StreamResult<Self> {
        let preparation = variant.prepare_reader(profile, content_time)?;
        Ok(Self {
            seek,
            signal,
            variant,
            active: AtomicBool::new(false),
            cancel: SessionCancel::new(cancel),
            construction_gate: ConstructionGate::default(),
            position: AtomicU64::new(0),
            readiness: SessionReadiness::Profiled {
                profile,
                preparation: Mutex::new(preparation),
            },
            transition: Some(transition),
            variant_index: transition.incoming_variant().get(),
        })
    }

    /// Whether the construction window this session was prepared for is
    /// readable.
    ///
    /// A pure question. It used to wake the peer on a negative answer, and that
    /// wake deadlocked the stream: this is called under the transition lock,
    /// while `wake_peer` takes the peer's state lock — and the peer takes those
    /// two in the opposite order, `poll_state_phase` holding its state lock
    /// across `prepare_for_seek` -> `cancel_incoming_for_seek`, which locks the
    /// transition. A seek epoch landing while an incoming session was being
    /// polled for readiness stopped the stream dead, with the queue full and
    /// nothing in flight.
    ///
    /// The wake is still owed — the variant plans nothing by itself, so without
    /// it an incoming session is only serviced when the *active* session happens
    /// to ask for bytes. It belongs to the caller, which knows when it has let
    /// the transition lock go. See [`wake_peer_for_readiness`].
    pub(crate) fn is_ready(&self) -> StreamResult<bool> {
        match &self.readiness {
            SessionReadiness::Active => Ok(true),
            SessionReadiness::Profiled { preparation, .. } => {
                self.variant.reader_is_ready(&preparation.lock())
            }
        }
    }

    pub(crate) fn position(&self) -> u64 {
        self.projected_position().byte
    }

    fn prepare_at(&self, position: Duration) -> StreamResult<SourceSeekAnchor> {
        if self.cancel.is_cancelled() {
            return Err(StreamError::Source(SourceError::Cancelled));
        }
        let SessionReadiness::Profiled {
            preparation,
            profile,
        } = &self.readiness
        else {
            return Err(StreamError::Source(SourceError::Io(Error::new(
                ErrorKind::Unsupported,
                "active HLS session does not carry a decoder reader profile",
            ))));
        };
        self.cancel.rearm();
        let next = self.variant.prepare_reader(*profile, position)?;
        let anchor = next.anchor();
        *preparation.lock() = next;
        self.set_position(anchor.byte_offset);
        self.arm_peer();
        Ok(anchor)
    }

    fn projected_position(&self) -> SessionPosition {
        let provisional = self.position.load(Ordering::Acquire);
        let Some(projection) = self.variant.resolved_seek_projection(provisional) else {
            return SessionPosition {
                byte: provisional,
                projection: None,
            };
        };
        let exact = projection.exact_anchor();
        self.variant.set_prefetch_anchor(exact);
        SessionPosition {
            byte: exact,
            projection: Some(projection),
        }
    }

    pub(crate) fn seek_time_anchor(
        &self,
        position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>> {
        let anchor = self.variant.prepare_seek_time_anchor(position)?;
        if let Some(anchor) = anchor {
            self.set_position(anchor.byte_offset);
        }
        Ok(anchor)
    }

    pub(crate) fn seek_to_byte(&self, position: u64) {
        let previous = self.position.swap(position, Ordering::AcqRel);
        let moved = previous != position;
        self.variant.register_session_seek(position, moved);
    }

    pub(crate) fn set_position(&self, position: u64) {
        self.position.store(position, Ordering::Release);
    }

    /// Whether the reader's own consumption has made a deferred prefetch
    /// decision stale. The session owns the byte cursor, so it is the session
    /// that answers this; the variant only owns the threshold.
    pub(crate) fn take_prefetch_resume(&self) -> bool {
        self.variant
            .take_prefetch_resume_at(self.position.load(Ordering::Acquire))
    }

    pub(crate) fn variant(&self) -> Arc<HlsVariant> {
        Arc::clone(&self.variant)
    }

    /// Demand-aware phase at the session's own read position. Names what the
    /// session's next byte is waiting on — `WaitingDemand` means a planned or
    /// in-flight fetch is servicing it, `Waiting` means nothing is.
    pub(crate) fn wait_phase(&self) -> SourcePhase {
        let byte = self.projected_position().byte;
        self.variant.phase_at(byte..byte.saturating_add(1))
    }

    /// Session-scoped twin of [`HlsCoord::wait_range`]: `Some(_)` is the
    /// wake-free RT probe, `None` the off-RT construction wait that parks on the
    /// readiness gate. The variant plans nothing by itself — the reader driver
    /// wakes the peer for the range — so the blocking probe notifies the peer
    /// before parking, the same consumer-wake shape `Stream::read` uses off-RT.
    pub(crate) fn wait_range(
        &self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        match timeout {
            Some(_) => self.variant.wait_range(range, timeout),
            None => HlsCoord::wait_range_blocking(&self.signal, &self.cancel.root, || {
                let outcome = self.variant.wait_range(range.clone(), Some(Duration::ZERO));
                if matches!(
                    outcome,
                    Err(StreamError::Source(SourceError::WaitBudgetExceeded))
                ) {
                    self.signal.wake_peer();
                }
                outcome
            }),
        }
    }
}

impl ByteMap for HlsSession {
    fn anchor_at_time(&self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        self.prepare_at(position).map(Some)
    }

    delegate::delegate! {
        to self.variant {
            #[call(init_byte_range)]
            fn init_segment_range(&self) -> Range<u64>;
            #[call(stream_len)]
            fn len(&self) -> Option<u64>;
            #[call(descriptor_after_byte)]
            fn segment_after_byte(&self, byte: u64) -> Option<SegmentDescriptor>;
            #[call(descriptor_at_byte)]
            fn segment_at_byte(&self, byte: u64) -> Option<SegmentDescriptor>;
            #[call(descriptor_at_time)]
            fn segment_at_time(&self, position: Duration) -> Option<SegmentDescriptor>;
            #[expr(Some($))]
            #[call(num_segments)]
            fn segment_count(&self) -> Option<u32>;
        }
    }

    fn segment_at_index(&self, segment_index: u32) -> Option<SegmentDescriptor> {
        self.variant.descriptor(segment_index as usize)
    }
}

fn pending(reason: PendingReason) -> Error {
    Error::new(ErrorKind::Interrupted, reason)
}
