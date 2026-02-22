#![forbid(unsafe_code)]

pub use crate::pool::{Pool, Pooled, PooledOwned, Reuse, SharedPool};

pub fn put<const SHARDS: usize, T>(pool: &Pool<SHARDS, T>, value: T, shard_idx: usize)
where
    T: Reuse,
{
    pool.put(value, shard_idx);
}

pub fn shard_index<const SHARDS: usize, T>(pool: &Pool<SHARDS, T>) -> usize
where
    T: Reuse,
{
    pool.shard_index()
}
