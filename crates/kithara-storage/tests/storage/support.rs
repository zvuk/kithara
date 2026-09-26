#[cfg(target_arch = "wasm32")]
use kithara_platform::CancelToken;
use kithara_storage::ResourceRead;
#[cfg(target_arch = "wasm32")]
use kithara_storage::{MemOptions, MemResource};
use kithara_test_utils::bufpool::pools;

/// Reads up to `len` bytes of `resource` at `offset` through a pooled buffer.
///
/// # Panics
///
/// Panics when the requested buffer exceeds the shared pool budget.
pub(crate) fn read_bytes<R: ResourceRead>(resource: &R, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = pools()
        .get_with_len::<u8>(len)
        .expect("read buffer fits the test pool budget");
    let read = resource.read_at(offset, &mut buf).unwrap_or(0);
    buf[..read].to_vec()
}

/// A committed in-memory resource pre-filled with `data`, the wasm stand-in
/// for a file already on disk.
///
/// # Panics
///
/// Panics if the in-memory resource rejects its initial data.
#[cfg(target_arch = "wasm32")]
pub(crate) fn mem_resource_with_bytes(data: &[u8], cancel: CancelToken) -> MemResource {
    MemResource::open(
        cancel,
        MemOptions::builder()
            .buffer(pools().get::<u8>())
            .initial_data(data.to_vec())
            .build(),
    )
    .expect("an in-memory resource with initial data opens")
}
