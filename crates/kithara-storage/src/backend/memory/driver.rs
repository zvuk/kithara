#![forbid(unsafe_code)]

use std::{fmt, sync::Arc};

use arc_swap::ArcSwapOption;
use kithara_bufpool::{BytePool, PooledOwned};
use kithara_platform::Mutex;
use rangemap::RangeSet;
use tokio_util::sync::CancellationToken;

/// Pooled byte buffer shared (via `Arc`) between the working state and the
/// committed snapshot.
pub(super) type SharedBuf = Arc<PooledOwned<32, Vec<u8>>>;

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
    /// Byte pool the driver allocates from (injected from config). Defaults to
    /// the process-wide [`BytePool`] for the top-level / test convenience
    /// constructors; the asset store passes its configured pool.
    pub byte_pool: BytePool,
}

/// Internal state of the growable memory driver.
pub(super) struct MemState {
    /// Pool-managed byte buffer, shared with the committed snapshot via `Arc`:
    /// `commit` is a zero-copy `Arc::clone`, so a committed resource holds a
    /// single pooled allocation. Writes are copy-on-write — if a snapshot or
    /// reader still holds the buffer, the write copies into a fresh pooled
    /// buffer, isolating a re-download from in-flight reads.
    pub(super) buf: SharedBuf,
    /// Logical length: highest write extent across all writes (for a committed
    /// resource, the committed length).
    pub(super) len: u64,
}

/// In-memory storage driver backed by a pooled, copy-on-write byte buffer.
///
/// `commit` publishes a zero-copy `Arc` snapshot for the lock-free read fast
/// path; the working buffer and the snapshot share one [`BytePool`] allocation.
/// The buffer returns to the pool (releasing its byte budget) when the last
/// `Arc` is dropped. Data is never evicted —
/// [`valid_window()`](crate::DriverIo::valid_window) returns `None`.
///
/// `path()` returns `None`.
pub struct MemDriver {
    pub(super) state: Mutex<MemState>,
    /// Immutable committed snapshot for the lock-free read fast path.
    pub(super) committed: ArcSwapOption<PooledOwned<32, Vec<u8>>>,
    /// Injected byte pool; all allocations (open, commit, copy-on-write) reuse
    /// this one handle rather than fetching the global accessor.
    pub(super) byte_pool: BytePool,
}

impl fmt::Debug for MemDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock_sync();
        f.debug_struct("MemDriver")
            .field("len", &state.len)
            .field("buf_len", &state.buf.len())
            .field("committed", &self.committed.load().is_some())
            .finish_non_exhaustive()
    }
}

impl Driver for MemDriver {
    type Options = MemOptions;

    fn open(opts: MemOptions) -> StorageResult<(Self, DriverState)> {
        let byte_pool = opts.byte_pool;
        let mut pooled = byte_pool.get();

        let (len, init_state, committed) = if let Some(data) = opts.initial_data {
            let data_len = data.len();
            if data_len > 0 {
                pooled
                    .ensure_len(data_len)
                    .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
                pooled[..data_len].copy_from_slice(&data);
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
            (
                len,
                DriverState {
                    available,
                    is_committed: true,
                    final_len: Some(len),
                },
                data_len,
            )
        } else {
            if opts.capacity > 0 {
                pooled
                    .ensure_len(opts.capacity)
                    .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
            }
            (0, DriverState::default(), 0)
        };

        // Share the buffer Arc with the committed snapshot when it carries
        // initial committed data: one pooled allocation, no copy. Zero-length
        // committed data publishes no snapshot, matching the mmap `Empty`
        // contract (`committed_len()` → `None`).
        let buf: SharedBuf = Arc::new(pooled);
        let committed = if committed == 0 {
            ArcSwapOption::empty()
        } else {
            ArcSwapOption::from(Some(Arc::clone(&buf)))
        };

        let driver = Self {
            state: Mutex::new(MemState { buf, len }),
            committed,
            byte_pool,
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
