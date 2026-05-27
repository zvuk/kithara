use std::time::Duration;

use kithara_events::{AbrMode, CancelReason, RequestId, RequestPriority};
use url::Url;

pub trait Probe {
    fn record_probe(&self, name: &'static str);
}

pub trait IntoProbeArg: Copy {
    #[must_use]
    fn from_probe_arg(packed: u64) -> Self {
        let _ = packed;
        unimplemented!("noop probe: from_probe_arg not supported without `probe` feature")
    }

    fn into_probe_arg(self) -> u64;
}

macro_rules! impl_int_probe_arg_noop {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IntoProbeArg for $ty {
                fn into_probe_arg(self) -> u64 { self as u64 }
                fn from_probe_arg(packed: u64) -> Self { packed as $ty }
            }
        )*
    };
}

impl_int_probe_arg_noop!(u64, i64, u32, i32, usize);

impl IntoProbeArg for bool {
    fn from_probe_arg(packed: u64) -> Self {
        packed != 0
    }
    fn into_probe_arg(self) -> u64 {
        u64::from(self)
    }
}

impl IntoProbeArg for Duration {
    fn from_probe_arg(packed: u64) -> Self {
        Self::from_micros(packed)
    }
    fn into_probe_arg(self) -> u64 {
        u64::try_from(self.as_micros()).unwrap_or(u64::MAX)
    }
}

impl IntoProbeArg for &Url {
    fn into_probe_arg(self) -> u64 {
        0
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
    match r {
        CancelReason::EpochCancel => 0,
        CancelReason::PeerCancel => 1,
        CancelReason::DownloaderShutdown => 2,
        CancelReason::BeforeStart => 3,
    }
}

impl IntoProbeArg for CancelReason {
    fn into_probe_arg(self) -> u64 {
        cancel_reason_wire(self)
    }
}

impl IntoProbeArg for AbrMode {
    fn into_probe_arg(self) -> u64 {
        usize::from(self) as u64
    }
}

impl<T: IntoProbeArg> IntoProbeArg for Option<T> {
    fn into_probe_arg(self) -> u64 {
        self.map_or(u64::MAX, IntoProbeArg::into_probe_arg)
    }
}

pub fn register_probes() {}

#[must_use]
pub fn caller_fn_above(_probe_fn_name: &str) -> Option<String> {
    None
}

#[must_use]
pub fn next_probe_seq() -> u64 {
    0
}

#[must_use]
pub fn next_thread_probe_seq() -> u64 {
    0
}

#[must_use]
pub fn current_thread_u64() -> u64 {
    0
}

#[must_use]
pub fn current_install_id() -> u64 {
    0
}

#[must_use]
pub fn bump_install_id() -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
kithara_platform::tokio::task_local! {
    pub static OWNED_INSTALL_ID: u64;
}

pub fn fire_0(_name: &'static str) {}
pub fn fire_1(_name: &'static str, _a0: u64) {}
pub fn fire_2(_name: &'static str, _a0: u64, _a1: u64) {}
pub fn fire_3(_name: &'static str, _a0: u64, _a1: u64, _a2: u64) {}
pub fn fire_4(_name: &'static str, _a0: u64, _a1: u64, _a2: u64, _a3: u64) {}
pub fn fire_5(_name: &'static str, _a0: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64) {}
pub fn fire_6(_name: &'static str, _a0: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) {}
