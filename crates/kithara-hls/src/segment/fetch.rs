use std::{
    io::Error as IoError,
    marker::PhantomData,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_assets::{AssetReader, AssetWriter, RawWriteHandle, ReadSide, WriteSide};
use kithara_drm::DecryptContext;
use kithara_net::{NetError, Retryability};
use kithara_platform::CancelToken;
use kithara_storage::ResourceStatus;
use kithara_stream::dl::{OnCompleteFn, WriterFn};
use tracing::{debug, error, warn};

use crate::{
    segment::state::{Downloading, Failed, Loaded, Missing, SegmentPhase, SegmentSlotState},
    signal::SizeSignal,
    variant::HlsVariant,
};

/// Whether a fetch error is terminal for the slot. The net layer's
/// resilient body already retried stalls and transient body errors, so a
/// `Fatal` error reaching the settle path means the downloader gave up —
/// the slot must be parked `Failed` rather than re-dispatched. `Cancelled`
/// is the one `Fatal` variant that stays recoverable: a cancel marks an
/// epoch rebuild, which owns the re-dispatch, so it keeps `Missing`.
fn is_terminal_fetch_error(e: &NetError) -> bool {
    !matches!(e, NetError::Cancelled) && e.retryability() == Retryability::Fatal
}

/// Phantom-typed handle to a segment / init slot. `S` is one of
/// [`Downloading`], [`Loaded`], [`Missing`]; the per-phase fields live in
/// `S::Data`. Transitions are consume-self methods on the phase-specific
/// `impl` blocks below, so the compiler rejects a double settle or an
/// [`apply_commit`](crate::variant::HlsVariant::apply_commit) on anything but a `Loaded`
/// handle.
pub(crate) struct FetchClaim<S: SegmentPhase> {
    data: S::Data,
    _phase: PhantomData<S>,
}

/// Backing payload of a [`FetchClaim<Downloading>`](FetchClaim). Shares the slot
/// CAS cell so a terminal transition can flip it, holds the `Weak`
/// back-reference for the post-commit size apply, and carries the `Drop`
/// disarm flag.
pub(crate) struct DownloadClaim {
    slot: Arc<SegmentSlotState>,
    planned: PlannedFetch,
    variant: Weak<HlsVariant>,
    settled: bool,
}

/// Backing payload of a [`FetchClaim<Loaded>`](FetchClaim): the committed slot
/// identity and resolved size consumed by [`HlsVariant::apply_commit`](crate::variant::HlsVariant::apply_commit).
pub(crate) struct LoadedProof {
    planned: PlannedFetch,
    final_len: u64,
}

impl FetchClaim<Downloading> {
    /// Build the owned in-flight handle after [`SegmentSlotState::try_claim`]
    /// wins the `Missing -> Downloading` CAS. `slot` shares the just-flipped
    /// CAS cell so a terminal transition can settle it; `variant` is the
    /// `Weak` back-reference for the post-commit size apply.
    pub(crate) fn claim(
        planned: PlannedFetch,
        variant: Weak<HlsVariant>,
        slot: Arc<SegmentSlotState>,
    ) -> Self {
        Self {
            data: DownloadClaim {
                planned,
                variant,
                slot,
                settled: false,
            },
            _phase: PhantomData,
        }
    }

    /// Consume the claim without touching slot state — used for a stale
    /// (cancelled) settle whose resource already committed: the new epoch
    /// owns the slot, so leaving it as-is is correct.
    pub(crate) fn abandon(mut self) {
        self.data.settled = true;
    }

    /// `Downloading -> Failed` terminal settle: the downloader exhausted
    /// its retry budget (the net layer's resilient body already retried
    /// the stall/transient errors), so the slot is parked permanently —
    /// `try_claim` will not re-dispatch it and a waiting reader surfaces a
    /// terminal error. Unlike [`into_missing`](Self::into_missing) the
    /// slot does NOT return to the dispatch pool.
    pub(crate) fn into_failed(mut self) -> FetchClaim<Failed> {
        self.data.slot.mark_failed();
        self.data.settled = true;
        FetchClaim {
            data: (),
            _phase: PhantomData,
        }
    }

    /// `Downloading -> Loaded` with a post-commit size apply. `actual` is
    /// the on-disk `final_len` (success / cache-hit / committed-by-race).
    /// `apply_commit` shrinks the variant's layout to match *before*
    /// `mark_loaded` flips the slot — a reader that observes `Loaded` then
    /// reads the size must never see the stale estimate.
    pub(crate) fn into_loaded(mut self, actual: u64) -> FetchClaim<Loaded> {
        let loaded = FetchClaim {
            data: LoadedProof {
                planned: self.data.planned,
                final_len: actual,
            },
            _phase: PhantomData,
        };
        if let Some(v) = self.data.variant.upgrade() {
            v.apply_commit(&loaded);
        }
        self.data.slot.mark_loaded();
        self.data.settled = true;
        loaded
    }

    /// `Downloading -> Loaded` without a size apply — the resource
    /// committed by a racing writer but reported no `final_len`, so the
    /// existing layout estimate stands.
    pub(crate) fn into_loaded_no_apply(mut self) -> FetchClaim<Loaded> {
        self.data.slot.mark_loaded();
        self.data.settled = true;
        FetchClaim {
            data: LoadedProof {
                planned: self.data.planned,
                final_len: 0,
            },
            _phase: PhantomData,
        }
    }

    /// `Downloading -> Missing` recovery (recoverable failure / cancel
    /// before commit). The slot returns to the dispatch pool.
    pub(crate) fn into_missing(mut self) -> FetchClaim<Missing> {
        self.data.slot.mark_missing();
        self.data.settled = true;
        FetchClaim {
            data: (),
            _phase: PhantomData,
        }
    }
}

impl FetchClaim<Loaded> {
    pub(crate) fn final_len(&self) -> u64 {
        self.data.final_len
    }

    pub(crate) fn planned(&self) -> PlannedFetch {
        self.data.planned
    }
}

/// The `Drop` safety net lives on the concrete payload (not on the generic
/// `FetchClaim<Downloading>`, which `Drop` cannot specialize): if a claim is
/// dropped without a transition, the slot reverts to `Missing` so a leaked
/// handle can never strand it in `Downloading`. The consume-self
/// transitions set `settled` first, disarming this no-op.
impl Drop for DownloadClaim {
    fn drop(&mut self) {
        if !self.settled {
            self.slot.mark_missing();
            warn!(
                target: "kithara_hls::settle",
                planned = ?self.planned,
                "Downloading claim dropped without settle — slot reverted to Missing"
            );
        }
    }
}

/// One unit of pending fetch work for the variant. `Init` is the only
/// non-segment entry — placed at the front of the queue by `rebuild` so
/// the fMP4 init prefix is fetched before any media segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlannedFetch {
    Init,
    Segment(u32),
}

/// Pairs the freshly-acquired [`AssetResource`](kithara_assets::AssetResource) with the entry's state
/// atom and the cancel token captured at dispatch time. `settle` reads
/// `cancel.is_cancelled()` as the rebuild-epoch marker: a stale fetch
/// (cancelled before completion) does not write to state — `rebuild`
/// has already taken over and the asset slot belongs to the new epoch.
///
/// The `Weak<HlsVariant>` lets the slot call back into the variant to
/// apply the post-decrypt size — we use `Weak` (not `Arc`) so a dropped
/// peer doesn't keep the variant alive past teardown.
pub(crate) struct FetchSlot {
    /// Unified reader-wake handle — [`SizeSignal::fire`]d on every terminal
    /// settle (commit/fail/cancel) so an off-RT reader parked in
    /// `wait_range(_, None)` re-probes the now-resolved range and the RT
    /// decoder's audio worker re-ticks the instant a commit makes bytes
    /// readable (the decrypt gate opens here for DRM segments), not on its
    /// 10 ms poll.
    pub(crate) signal: SizeSignal,
    /// Read view of the writer's generation — used to observe a
    /// committed-by-race status before deciding the terminal transition.
    pub(crate) reader: AssetReader<DecryptContext>,
    /// Sole commit owner (non-`Clone`); consumed in `settle`.
    pub(crate) writer: AssetWriter<DecryptContext>,
    pub(crate) cancel: CancelToken,
    /// Clone-able streaming-write handle for the fetch body closure.
    pub(crate) raw: RawWriteHandle,
    pub(crate) handle: FetchClaim<Downloading>,
}

impl From<FetchSlot> for OnCompleteFn {
    fn from(slot: FetchSlot) -> Self {
        Box::new(move |bytes_written, _headers, err| slot.settle(bytes_written, err))
    }
}

impl FetchSlot {
    /// On success, commits the resource. `bytes_written` is forwarded as
    /// `final_len` — required by [`ProcessedResource::commit`] to trigger
    /// the post-write decrypt pass on encrypted segments (passing `None`
    /// silently skips decryption, leaving ciphertext on disk). After a
    /// successful commit we read back the resource's `final_len` and
    /// shrink the variant's layout to match: for DRM segments PKCS7
    /// strips up to 16 bytes off the encrypted size, so HEAD-based
    /// estimates are always upper bounds.
    ///
    /// Consumes the slot (`OnCompleteFn` is `FnOnce`): the owned
    /// [`FetchClaim<Downloading>`](FetchClaim) handle is moved into exactly one
    /// terminal transition, so the slot state can never be double-driven.
    fn settle(self, bytes_written: u64, err: Option<&NetError>) {
        // Wake any reader parked on this range AFTER the terminal transition
        // (commit makes bytes readable / fail flips `range_has_failed`). The
        // worker wake re-ticks the RT decoder's audio worker too — for DRM the
        // decrypted bytes only become readable at this commit, so settle (not
        // the ciphertext write) is the load-bearing wake.
        let signal = self.signal.clone();
        self.settle_inner(bytes_written, err);
        signal.fire();
    }

    fn settle_cancelled(self, bytes_written: u64) {
        let Self {
            handle,
            writer,
            reader,
            ..
        } = self;
        let committed = matches!(reader.status(), ResourceStatus::Committed { .. });
        debug!(target: "kithara_hls::settle", bytes_written, committed, "stale (cancelled)");
        if committed {
            // Committed by the new epoch's writer — dropping our (stale)
            // writer fails only its own generation's gate; the cleanup is
            // race-safe (skips removal when the live state is Committed).
            drop(writer);
            handle.abandon();
        } else {
            writer.fail("fetch cancelled before completion".into());
            handle.into_missing();
        }
    }

    fn settle_failure(self, e: &NetError) {
        let Self {
            handle,
            writer,
            reader,
            ..
        } = self;
        let committed = matches!(reader.status(), ResourceStatus::Committed { .. });
        debug!(target: "kithara_hls::settle", err = %e, committed, "fail-path");
        if committed {
            // Committed by the new epoch's writer; ours never wrote — drop it
            // (cleanup is race-safe) and adopt the on-disk length.
            drop(writer);
            if let ResourceStatus::Committed { final_len: Some(n) } = reader.status() {
                handle.into_loaded(n);
            } else {
                handle.into_loaded_no_apply();
            }
        } else {
            writer.fail(e.to_string());
            // The net layer's resilient body already retried stalls and
            // transient body errors; a `Fatal` error here (e.g.
            // `RetryExhausted`) means the downloader gave up, so the slot
            // is terminal — parking it as `Failed` stops the re-dispatch
            // loop and lets a waiting reader surface a terminal error.
            // `Cancelled` is recoverable (epoch rebuild owns it) and stays
            // `Missing`. The typed cause is logged here; readers see only
            // the fixed terminal message (no transport detail).
            if is_terminal_fetch_error(e) {
                error!(
                    target: "kithara_hls::settle",
                    err = %e,
                    "terminal fetch failure — slot parked Failed, will not re-dispatch"
                );
                handle.into_failed();
            } else {
                handle.into_missing();
            }
        }
    }

    fn settle_inner(self, bytes_written: u64, err: Option<&NetError>) {
        if self.cancel.is_cancelled() {
            self.settle_cancelled(bytes_written);
            return;
        }
        match err {
            None => self.settle_success(bytes_written),
            Some(e) => self.settle_failure(e),
        }
    }

    fn settle_success(self, bytes_written: u64) {
        let Self { handle, writer, .. } = self;
        // Consume-self commit returns the Ready reader; read `final_len` off it
        // (PKCS7 unpad shrinks DRM segments below the HEAD estimate).
        match writer.commit(Some(bytes_written)) {
            Ok(reader) => {
                debug!(target: "kithara_hls::settle", bytes_written, "success");
                let actual = match reader.status() {
                    ResourceStatus::Committed { final_len: Some(n) } => n,
                    _ => bytes_written,
                };
                handle.into_loaded(actual);
            }
            Err(e) => {
                debug!(
                    target: "kithara_hls::settle",
                    bytes_written,
                    err = %e,
                    "success-but-commit-failed"
                );
                handle.into_missing();
            }
        }
    }

    pub(crate) fn writer(&self) -> WriterFn {
        let raw = self.raw.clone();
        let offset = Arc::new(AtomicU64::new(0));
        Box::new(move |chunk: &[u8]| {
            let pos = offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            raw.write_at(pos, chunk).map_err(IoError::other)
        })
    }
}
