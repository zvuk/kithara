#![forbid(unsafe_code)]

use std::{fmt, sync::Arc};

use arc_swap::ArcSwapOption;
use kithara_bufpool::{BytePool, PooledOwned};
use kithara_platform::Mutex;
use rangemap::RangeSet;
use tokio_util::sync::CancellationToken;

use crate::{
    StorageError, StorageResult,
    backend::{
        resource::ResourceWriter,
        traits::{Driver, DriverState},
    },
};

/// Options for creating a [`MemResource`].
#[derive(Debug, Clone, Default)]
pub struct MemOptions {
    /// Pre-fill the resource with this data (committed on creation).
    pub initial_data: Option<Vec<u8>>,
    /// Initial capacity hint in bytes.
    /// The buffer starts with this capacity but grows as needed on writes.
    /// Defaults to 0 (start empty, grow on demand).
    pub capacity: usize,
}

/// Internal state of the growable memory driver.
pub(super) struct MemState {
    /// Pool-managed byte buffer. Grows via `ensure_len()`.
    pub(super) buf: PooledOwned<32, Vec<u8>>,
    /// Logical length: highest write extent across all writes.
    pub(super) len: u64,
}

/// In-memory storage driver backed by a growable byte pool buffer.
///
/// Uses [`BytePool::default()`](kithara_bufpool::byte_pool) for memory management
/// with byte budget enforcement. Data is never evicted —
/// [`valid_window()`](crate::DriverIo::valid_window) returns `None`.
///
/// `path()` returns `None`.
pub struct MemDriver {
    pub(super) state: Mutex<MemState>,
    /// Immutable committed snapshot for the lock-free read fast path.
    pub(super) committed: ArcSwapOption<Vec<u8>>,
}

impl fmt::Debug for MemDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock_sync();
        f.debug_struct("MemDriver")
            .field("len", &state.len)
            .field("capacity", &state.buf.capacity())
            .field("committed", &self.committed.load().is_some())
            .finish()
    }
}

impl Driver for MemDriver {
    type Options = MemOptions;

    fn open(opts: MemOptions) -> StorageResult<(Self, DriverState)> {
        let mut buf = BytePool::default().get();

        let (len, init_state, committed) = if let Some(data) = opts.initial_data {
            let data_len = data.len();
            if data_len > 0 {
                buf.ensure_len(data_len)
                    .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
                buf[..data_len].copy_from_slice(&data);
            }
            let len = u64::try_from(data_len).map_err(|err| {
                StorageError::Failed(format!(
                    "memory open: initial len {data_len} does not fit u64: {err}"
                ))
            })?;
            let mut available = RangeSet::new();
            if len > 0 {
                available.insert(0..len);
            }
            // Zero-length committed data publishes no snapshot, matching the
            // mmap `Empty` contract (`committed_len()` → `None`).
            let committed = if data.is_empty() {
                ArcSwapOption::empty()
            } else {
                ArcSwapOption::from(Some(Arc::new(data)))
            };
            (
                len,
                DriverState {
                    available,
                    is_committed: true,
                    final_len: Some(len),
                },
                committed,
            )
        } else {
            if opts.capacity > 0 {
                buf.ensure_len(opts.capacity)
                    .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
            }
            (0, DriverState::default(), ArcSwapOption::empty())
        };

        let driver = Self {
            state: Mutex::new(MemState { buf, len }),
            committed,
        };

        Ok((driver, init_state))
    }
}

/// In-memory storage resource.
///
/// Type alias for [`ResourceWriter<MemDriver>`]. Drop-in replacement for
/// [`MmapResource`](crate::MmapResource) on platforms without filesystem access.
pub type MemResource = ResourceWriter<MemDriver>;

impl MemResource {
    /// Create a new empty in-memory resource.
    ///
    /// # Panics
    ///
    /// Panics if `MemDriver::open` fails (should never happen with default options).
    #[must_use]
    pub fn new(cancel: CancellationToken) -> Self {
        Self::open(cancel, MemOptions::default())
            .expect("BUG: MemDriver::open with default options is infallible")
    }
}
