use kithara::{
    assets::{AssetReader, ReadSide},
    platform::CancelToken,
    storage::{
        DriverIo, MemOptions, MemResource, Resource, ResourcePhase, ResourceRead, StorageResult,
    },
};

use crate::bufpool_ext::{TestPools, pools};

pub trait PooledRead {
    /// Reads bytes starting at `offset` into `buf`.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the underlying resource cannot read the
    /// requested range.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
}

impl<P: ResourcePhase, D: DriverIo> PooledRead for Resource<P, D>
where
    Self: ResourceRead,
{
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        ResourceRead::read_at(self, offset, buf)
    }
}

impl PooledRead for AssetReader<TestPools> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        ReadSide::read_at(self, offset, buf)
    }
}

/// Reads up to `len` bytes from a pooled resource.
///
/// # Panics
///
/// Panics when the requested test buffer exceeds the shared pool budget.
pub fn read_bytes<R: PooledRead>(resource: &R, offset: u64, len: usize) -> Vec<u8> {
    let pools = pools();
    let mut buf = pools
        .get_with_len::<u8>(len)
        .expect("read buffer fits the test pool budget");
    let read = resource.read_at(offset, &mut buf).unwrap_or(0);
    buf[..read].to_vec()
}

/// Build a committed in-memory resource pre-filled with `data`.
///
/// Mirrors the old `MemResource::with_bytes` test constructor over the
/// public `MemResource::open` API.
///
/// # Panics
///
/// Panics if the in-memory resource rejects its initial data.
#[must_use]
pub fn mem_resource_with_bytes(data: &[u8], cancel: CancelToken) -> MemResource {
    MemResource::open(
        cancel,
        MemOptions::builder()
            .buffer(pools().get::<u8>())
            .initial_data(data.to_vec())
            .build(),
    )
    .expect("BUG: MemDriver::open with initial_data is infallible")
}
