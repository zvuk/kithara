//! `IntoProbeArg` + `Probe` traits and their stock impls.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::Duration,
};

use kithara_events::{CancelReason, RequestId, RequestPriority};
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
        u64::try_from(self.as_micros()).unwrap_or(u64::MAX) // ast-grep-ignore: rust.no-sentinel-fallback
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
/// from multiple init paths — guarded by an internal `OnceLock`. On
/// wasm32 this is a no-op.
pub fn register_probes() {
    imp::register();
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::OnceLock;

    static REGISTERED: OnceLock<()> = OnceLock::new();

    pub(super) fn register() {
        REGISTERED.get_or_init(|| {
            let _ = usdt::register_probes();
        });
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    pub(super) fn register() {}
}
