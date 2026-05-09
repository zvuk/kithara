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
}

impl IntoProbeArg for u64 {
    fn into_probe_arg(self) -> u64 {
        self
    }
}

impl IntoProbeArg for i64 {
    fn into_probe_arg(self) -> u64 {
        u64::from_ne_bytes(self.to_ne_bytes())
    }
}

impl IntoProbeArg for u32 {
    fn into_probe_arg(self) -> u64 {
        u64::from(self)
    }
}

impl IntoProbeArg for i32 {
    fn into_probe_arg(self) -> u64 {
        u64::from_ne_bytes(i64::from(self).to_ne_bytes())
    }
}

impl IntoProbeArg for usize {
    fn into_probe_arg(self) -> u64 {
        self as u64
    }
}

impl IntoProbeArg for bool {
    fn into_probe_arg(self) -> u64 {
        u64::from(self)
    }
}

impl IntoProbeArg for Duration {
    fn into_probe_arg(self) -> u64 {
        u64::try_from(self.as_micros()).unwrap_or(u64::MAX)
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
