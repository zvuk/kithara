use std::num::NonZeroUsize;

use kithara_platform::time::Duration;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const LAYER_ROOT: &str = "test_asset";

/// Default [`ProcessedWriter`] transform pass size. One pass over a 64 `KiB`
/// window keeps the pooled input and output buffers page-sized while still
/// committing most resources in a handful of passes.
pub(crate) const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;

/// Default backstop between [`ReadinessGate`] wakeups. The gate is woken by
/// `notify_all` on every state change, so this only bounds how long a waiter
/// sleeps before rechecking an abort it was not signalled for.
pub(crate) const DEFAULT_GATE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The asset root an absolute key is filed under.
pub(crate) const ABSOLUTE_ROOT: &str = "__absolute__";

/// Capacity of each retire queue. It buys time, not a bound: no capacity can
/// span an unbounded read:write ratio, so raising this number only moves the
/// overflow threshold.
pub(crate) const RETIRE_CAPACITY: usize = 256;

pub(crate) const DEFAULT_EXTENSION: &str = "bin";
pub(crate) const MAX_EXTENSION_LEN: usize = 16;
pub(crate) const HASH_BYTES: usize = 16;

/// Windows hashes a path as wide units under a domain of its own, so this
/// one names no platform there.
#[cfg(not(windows))]
pub(crate) const LOCAL_UNIX_DOMAIN: &[u8] = b"kithara.asset-root.local.unix.v1\0";

#[cfg(windows)]
pub(crate) const LOCAL_WINDOWS_DOMAIN: &[u8] = b"kithara.asset-root.local.windows.v1\0";

pub(crate) const REMOTE_DOMAIN: &[u8] = b"kithara.asset-root.remote.v1\0";
pub(crate) const MAX_COMPONENT_LEN: usize = 96;
pub(crate) const HASH_PREFIX_BYTES: usize = 16;

/// Default in-memory LRU cache capacity (init + 2-3 media segments).
pub(crate) const DEFAULT_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap();

#[cfg(test)]
pub(crate) const BUILDER_ROOT: &str = "test_asset";
