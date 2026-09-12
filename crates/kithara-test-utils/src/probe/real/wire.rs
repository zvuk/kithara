use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use kithara_platform::time::Duration;
use url::Url;

/// Implemented by `#[derive(kithara::Probe)]` for value-type probe payloads.
pub trait Probe {
    /// Fire the probe associated with this value.
    fn record_probe(&self, name: &'static str, operation: u64);
}

/// Stable, allocation-free USDT operation identifier.
///
/// The provider has room for six `u64` values. Every firing reserves the
/// first one for this FNV-1a hash; the remaining five are operation payload.
/// Callers pass a `concat!(module_path!(), "::", operation)` literal, so the
/// hash is evaluated at compile time and does not touch the RT path.
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

/// Convert a value of arbitrary type into the `u64` USDT wire format.
///
/// `Self: Copy` is required so the `#[probe]` macro can pass arguments
/// by value without forcing call-sites to clone non-`Copy` payloads.
pub trait IntoProbeArg: Copy {
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

    /// Encode `self` as a u64 probe argument.
    fn into_probe_arg(self) -> u64;
}

/// Generate a round-trippable [`IntoProbeArg`] impl for an integer type.
/// Wire shape is `u64`; `AsPrimitive` reproduces the `as`-cast semantics
/// (zero-extension for unsigned, two's-complement round-trip for signed)
/// without tripping clippy's truncation/sign-loss lints.
macro_rules! impl_int_probe_arg {
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

impl_int_probe_arg!(u64, i64, u32, i32, usize);

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
        let mut hasher = DefaultHasher::new();
        self.as_str().hash(&mut hasher);
        hasher.finish()
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

/// Register the macOS DTrace probes embedded in the binary. Other targets use
/// the tracing USDT backend and do not require registration.
pub fn register_probes() {
    imp::register();
}

#[cfg(all(target_os = "macos", feature = "usdt", not(miri)))]
mod imp {
    use std::sync::OnceLock;

    static REGISTERED: OnceLock<()> = OnceLock::new();

    pub(super) fn register() {
        REGISTERED.get_or_init(|| {
            let _ = usdt::register_probes();
        });
    }
}

#[cfg(any(not(target_os = "macos"), not(feature = "usdt"), miri))]
mod imp {
    pub(super) fn register() {}
}
