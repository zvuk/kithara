use bytes::Bytes;
use kithara_bufpool::{ByteBuffer, HasPool, PoolError, PoolRegion};
use kithara_platform::sync::Arc;

struct PooledBytes {
    bytes: ByteBuffer,
}

impl AsRef<[u8]> for PooledBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone)]
pub(crate) struct ByteBuffers {
    get_with_len: Arc<dyn Fn(usize) -> Result<ByteBuffer, PoolError> + Send + Sync>,
}

impl ByteBuffers {
    pub(crate) fn new<S>(pools: PoolRegion<S>) -> Self
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        Self {
            get_with_len: Arc::new(move |len| pools.get_with_len::<u8>(len)),
        }
    }

    pub(crate) fn get_with_len(&self, len: usize) -> Result<ByteBuffer, PoolError> {
        (self.get_with_len)(len)
    }
}

pub(crate) fn pooled_bytes(bytes: ByteBuffer) -> Bytes {
    Bytes::from_owner(PooledBytes { bytes })
}
