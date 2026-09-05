use kithara::{
    assets::{AssetReader, ReadSide},
    platform::CancelToken,
    storage::{
        DriverIo, MemOptions, MemResource, Resource, ResourcePhase, ResourceRead, StorageResult,
    },
};

use crate::bufpool_ext::{TestPools, pools};

pub trait PooledRead {
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
