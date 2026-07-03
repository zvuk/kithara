use kithara::bufpool::{Pool, Reuse};

pub trait PoolShardTestExt {
    /// Instance-form of [`Pool::shard_index`] so tests holding a `&Pool`
    /// can read the calling thread's home shard without spelling out the
    /// generics.
    fn shard_index_of(&self) -> usize;
}

impl<const SHARDS: usize, T: Reuse> PoolShardTestExt for Pool<SHARDS, T> {
    fn shard_index_of(&self) -> usize {
        Self::shard_index()
    }
}
