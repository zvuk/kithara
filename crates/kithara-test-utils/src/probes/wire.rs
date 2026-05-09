//! `IntoProbeArg` + `Probe` traits and their stock impls.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::Duration,
};

use kithara_events::{AbrMode, CancelReason, RequestId, RequestPriority};
use url::Url;

/// Implemented by `#[derive(kithara::Probe)]` for value-type probe payloads.
pub trait Probe {
    /// Fire the probe associated with this value. `name` becomes the
    /// `probe` field on the tracing event so call-site granularity
    /// survives even though the USDT probe name is fixed.
    fn record_probe(&self, name: &'static str);
}

/// Convert a value of arbitrary type into the `u64` USDT wire format.
///
/// `Self: Copy` is required so the `#[probe]` macro can pass arguments
/// by value without forcing call-sites to clone non-`Copy` payloads.
pub trait IntoProbeArg: Copy {
    /// Encode `self` as a u64 probe argument.
    fn into_probe_arg(self) -> u64;

    /// Decode a packed `u64` back into `Self`. Default panics with the
    /// type name — override on every type whose `into_probe_arg` is
    /// expected to round-trip (scalars, `Duration`, `RequestId`, etc.)
    /// or whose probe payload tests want to inspect by field
    /// (multi-field packers such as `SegmentRequest`). Loose packers
    /// that drop bits may return a partial value with sentinel-filled
    /// fields — document the lossy fields on the impl.
    ///
    /// Tests should call `T::from_probe_arg(event.u64("name").unwrap())`
    /// instead of writing private decode helpers next to `IntoProbeArg`
    /// impls.
    #[must_use]
    fn from_probe_arg(packed: u64) -> Self {
        let _ = packed;
        unimplemented!(
            "{} did not implement IntoProbeArg::from_probe_arg — \
             override the trait method on the type whose packed `u64` \
             you are trying to decode (or the test reads the wrong field)",
            std::any::type_name::<Self>(),
        )
    }
}

/// Generate a round-trippable [`IntoProbeArg`] impl for an integer type
/// whose `as u64` / `as Self` cast is the canonical wire shape.
/// `as` casts are well-defined for all integer-to-integer in Rust:
/// signed values round-trip via two's complement, unsigned via zero
/// extension. Each scalar `IntoProbeArg` impl below is exactly one
/// `impl_int_probe_arg!(...)` line — saves the eight-line per-type
/// repetition the trait would otherwise demand.
macro_rules! impl_int_probe_arg {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IntoProbeArg for $ty {
                fn into_probe_arg(self) -> u64 {
                    self as u64
                }
                fn from_probe_arg(packed: u64) -> Self {
                    packed as $ty
                }
            }
        )*
    };
}

impl_int_probe_arg!(u64, i64, u32, i32, usize);

impl IntoProbeArg for bool {
    fn into_probe_arg(self) -> u64 {
        u64::from(self)
    }
    fn from_probe_arg(packed: u64) -> Self {
        packed != 0
    }
}

impl IntoProbeArg for Duration {
    fn into_probe_arg(self) -> u64 {
        u64::try_from(self.as_micros()).unwrap_or(u64::MAX)
    }
    fn from_probe_arg(packed: u64) -> Self {
        Self::from_micros(packed)
    }
}

impl IntoProbeArg for &Url {
    fn into_probe_arg(self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.as_str().hash(&mut hasher);
        hasher.finish()
    }
}

impl IntoProbeArg for RequestId {
    fn into_probe_arg(self) -> u64 {
        self.get()
    }
}

fn request_priority_wire(p: RequestPriority) -> u64 {
    match p {
        RequestPriority::High => 0,
        RequestPriority::Low => 1,
    }
}

impl IntoProbeArg for RequestPriority {
    fn into_probe_arg(self) -> u64 {
        request_priority_wire(self)
    }
}

fn cancel_reason_wire(r: CancelReason) -> u64 {
    const EPOCH_CANCEL: u64 = 0;
    const PEER_CANCEL: u64 = 1;
    const DOWNLOADER_SHUTDOWN: u64 = 2;
    const BEFORE_START: u64 = 3;
    match r {
        CancelReason::EpochCancel => EPOCH_CANCEL,
        CancelReason::PeerCancel => PEER_CANCEL,
        CancelReason::DownloaderShutdown => DOWNLOADER_SHUTDOWN,
        CancelReason::BeforeStart => BEFORE_START,
    }
}

impl IntoProbeArg for CancelReason {
    fn into_probe_arg(self) -> u64 {
        cancel_reason_wire(self)
    }
}

impl IntoProbeArg for AbrMode {
    fn into_probe_arg(self) -> u64 {
        // Same wire encoding as `From<AbrMode> for usize`:
        // `Manual(v)` → `v`, `Auto(None)` → `usize::MAX`,
        // `Auto(Some(v))` → `usize::MAX - 1 - v`. Tests filter on
        // the `Manual(idx)` case where the encoding is just `idx`.
        usize::from(self) as u64
    }
}

impl<T: IntoProbeArg> IntoProbeArg for Option<T> {
    fn into_probe_arg(self) -> u64 {
        self.map_or(u64::MAX, |value| {
            let raw = value.into_probe_arg();
            debug_assert!(
                raw != u64::MAX,
                "Option<T>::None sentinel collides with Some(value) producing u64::MAX"
            );
            raw
        })
    }
}

/// Register all USDT probes embedded in the binary with the host
/// kernel tracer (dtrace on macOS, bpftrace on Linux). Safe to call
/// from multiple init paths — guarded by an internal `OnceLock`. На
/// wasm32 и в прод-сборке (`feature = "test-utils"` выключена) это
/// no-op stub — `usdt`-крейт оптуально и не пуллится.
pub fn register_probes() {
    imp::register();
}

/// Resolve the symbol name of the function that called the probe-
/// attributed function `probe_fn_name`.
///
/// Walks `backtrace::Backtrace`, skips frames inside the probe machinery
/// (`kithara_test_utils::probes::*`) and the probe-attributed frame
/// itself, and returns the first remaining symbol — that is the
/// production-code caller. Returns `None` when frames are unavailable
/// (target=wasm32 with `backtrace` disabled, debug info stripped, etc.).
///
/// Used by the `#[kithara::probe]` expansion to record `caller_fn` on
/// every event so tests can assert on call-site identity by symbol
/// name (`assert_eq!(evt.caller_fn(), Some("…::format_change_segment_range"))`)
/// rather than by fragile `file.rs:line` strings.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn caller_fn_above(probe_fn_name: &str) -> Option<String> {
    // Hot-path consideration: this runs on every `#[kithara::probe]`
    // firing, including very-frequent ones like `Future::poll_next`.
    // `backtrace::Backtrace::new()` resolves *every* frame on the
    // stack — that's tens of frames × demangling each call, easily
    // >1ms per fire. We use the lower-level `backtrace::trace` +
    // `resolve_frame` API so we can early-return as soon as the
    // first non-machinery frame past the probe-attributed function
    // is resolved.
    let mut found_self = false;
    let mut result: Option<String> = None;
    backtrace::trace(|frame| {
        if result.is_some() {
            return false;
        }
        let mut symbol_seen = false;
        backtrace::resolve_frame(frame, |symbol| {
            if result.is_some() || symbol_seen {
                return;
            }
            symbol_seen = true;
            let Some(raw_name) = symbol.name() else {
                return;
            };
            let demangled = format!("{raw_name}");
            let trimmed = demangled
                .rsplit_once("::h")
                .map_or(demangled.as_str(), |(head, _)| head);
            if trimmed.starts_with("kithara_test_utils::probes::")
                || trimmed.starts_with("backtrace::")
                || trimmed.starts_with("std::backtrace::")
            {
                return;
            }
            if !found_self {
                if trimmed.contains(probe_fn_name) {
                    found_self = true;
                }
                return;
            }
            result = Some(trimmed.to_string());
        });
        result.is_none()
    });
    result
}

#[cfg(target_arch = "wasm32")]
pub fn caller_fn_above(_probe_fn_name: &str) -> Option<String> {
    None
}

/// Process-wide monotonic sequence number for probe firings.
///
/// Used by the `#[kithara::probe]` macro to attach a deterministic
/// ordering field (`seq`) to every emitted tracing event. `Instant`-
/// based ordering breaks down when two probes fire within the same
/// nanosecond on different threads; a per-process atomic counter
/// closes that gap and lets tests assert on event ordering even when
/// timestamps tie. `Ordering::Relaxed` is sufficient — uniqueness is
/// the only invariant; consumers that need cross-thread happens-before
/// must synchronise through a different channel.
pub fn next_probe_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Per-thread monotonic sequence number for probe firings.
///
/// Recorded alongside the global `seq` so a test can reconstruct the
/// **per-thread call order** without resorting to the global ordering
/// (which interleaves unrelated work). Together with [`current_thread_u64`]
/// this lets a test pin down "on thread T the i-th probe was X with args
/// (...)" and fail the test the moment the i-th probe diverges from the
/// expected one — instead of timing out on a `wait_for_probe` and leaving
/// the operator to scan logs.
#[must_use]
pub fn next_thread_probe_seq() -> u64 {
    use std::cell::Cell;
    thread_local! {
        static SEQ: Cell<u64> = const { Cell::new(0) };
    }
    SEQ.with(|cell| {
        let v = cell.get();
        cell.set(v.wrapping_add(1));
        v
    })
}

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::tokio;

/// Process-wide install-generation counter.
///
/// Bumped once per [`bump_install_id`]; the probe macro stamps the
/// **owning** `install_id` (from the `OWNED_INSTALL_ID` task-local) into
/// every emitted event, and `Recorder::snapshot` filters by its
/// captured `install_id`.
///
/// Why a task-local on top of a global atomic: orphan async tasks
/// from a just-finished test (downloader on-complete closures still
/// resolving HTTP, audio worker `write_all`'ing the last buffer) that
/// outlive their test's `Drop` would otherwise read the *current*
/// global at fire-time — which is the *next* test's id — and leak
/// into the next recorder's snapshot. With `OWNED_INSTALL_ID` set in
/// scope by `#[kithara::test]` and inherited by `tokio::spawn`, those
/// orphans freeze their own id at task-creation time and the new
/// recorder filters them out.
///
/// Ordering is `Relaxed`: uniqueness is the only invariant. Tests
/// observe their own events through the `OWNED_INSTALL_ID` scope,
/// which is set *before* the test body's first probe site.
static INSTALL_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(not(target_arch = "wasm32"))]
tokio::task_local! {
    /// Per-test install_id captured at scope entry. Inherited by
    /// `tokio::spawn` automatically; not inherited by `spawn_blocking`
    /// — that path falls back to the global atomic, which is the
    /// best we can do for non-tokio threads.
    pub static OWNED_INSTALL_ID: u64;
}

/// Read the `install_id` of the current test.
///
/// Prefers the task-local set by `#[kithara::test]`; falls back to
/// the global atomic for code paths outside any test scope (notably
/// `spawn_blocking` workers and pre-test probe firings before the
/// first test scope is entered).
#[must_use]
pub fn current_install_id() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Ok(id) = OWNED_INSTALL_ID.try_with(|id| *id) {
            return id;
        }
    }
    INSTALL_ID.load(AtomicOrdering::Relaxed)
}

/// Bump the global install-generation counter and return the new value.
///
/// Called once per test by the `#[kithara::test]` macro before the test
/// body enters `OWNED_INSTALL_ID.scope(...)`. `probe_capture::install()`
/// reads the task-local; it does not bump.
#[must_use]
pub fn bump_install_id() -> u64 {
    INSTALL_ID.fetch_add(1, AtomicOrdering::Relaxed) + 1
}

/// Numeric identifier of the calling OS thread.
///
/// `std::thread::ThreadId` carries a private `NonZeroU64` (`as_u64()` is
/// nightly-only) so we hash its `Debug` form. Equal `ThreadId` values
/// hash to the same `u64`, distinct values almost certainly do not —
/// the only consumer is "group probe events by thread" and a hash
/// collision would fold two threads into one bucket; in 50-thread test
/// runs that's a 64-bit-birthday non-event.
#[must_use]
pub fn current_thread_u64() -> u64 {
    let id = std::thread::current().id();
    let mut hasher = DefaultHasher::new();
    format!("{id:?}").hash(&mut hasher);
    hasher.finish()
}

#[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
mod imp {
    use std::sync::OnceLock;

    static REGISTERED: OnceLock<()> = OnceLock::new();

    pub(super) fn register() {
        REGISTERED.get_or_init(|| {
            let _ = usdt::register_probes();
        });
    }
}

#[cfg(any(target_arch = "wasm32", not(feature = "test-utils")))]
mod imp {
    pub(super) fn register() {}
}
