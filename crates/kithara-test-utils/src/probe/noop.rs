use kithara_platform::time::Duration;
use url::Url;

pub trait Probe {
    fn record_probe(&self, name: &'static str, operation: u64);
}

#[must_use]
pub const fn operation_id(name: &str) -> u64 {
    let bytes = name.as_bytes();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        index += 1;
    }
    hash
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
                fn into_probe_arg(self) -> u64 {
                    num_traits::AsPrimitive::<u64>::as_(self)
                }
                fn from_probe_arg(packed: u64) -> Self {
                    num_traits::AsPrimitive::<Self>::as_(packed)
                }
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

impl<T: IntoProbeArg> IntoProbeArg for Option<T> {
    fn into_probe_arg(self) -> u64 {
        self.map_or(u64::MAX, IntoProbeArg::into_probe_arg)
    }
}

pub fn register_probes() {}

pub fn fire_0(_operation: u64) {}
pub fn fire_1(_operation: u64, _a0: u64) {}
pub fn fire_2(_operation: u64, _a0: u64, _a1: u64) {}
pub fn fire_3(_operation: u64, _a0: u64, _a1: u64, _a2: u64) {}
pub fn fire_4(_operation: u64, _a0: u64, _a1: u64, _a2: u64, _a3: u64) {}
pub fn fire_5(_operation: u64, _a0: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64) {}
