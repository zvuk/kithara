use std::{mem, ops::RangeBounds};

use kithara_platform::sync::Arc;

use crate::{
    PoolError,
    pool::{Core, storage::Storage},
};

pub(crate) struct OwnedBuffer<const SHARDS: usize, B, const OBSERVE: bool>
where
    B: Storage,
{
    core: Arc<Core<SHARDS, B, OBSERVE>>,
    shard_idx: usize,
    pub(super) value: B,
}

impl<const SHARDS: usize, B, const OBSERVE: bool> OwnedBuffer<SHARDS, B, OBSERVE>
where
    B: Storage,
{
    pub(crate) fn new(core: Arc<Core<SHARDS, B, OBSERVE>>, value: B, shard_idx: usize) -> Self {
        Self {
            core,
            shard_idx,
            value,
        }
    }

    delegate::delegate! {
        to self.value {
            pub(super) fn capacity(&self) -> usize;
            pub(super) fn clear(&mut self);
        }
    }

    pub(super) fn renew(&mut self) {
        let value = mem::take(&mut self.value);
        self.core.put(value, self.shard_idx);
    }

    pub(super) fn normalize(&mut self) {
        self.core.normalize(&mut self.value, self.shard_idx);
    }

    fn grow(&mut self, new_len: usize) -> Result<(), PoolError> {
        self.core.grow(&mut self.value, new_len, self.shard_idx)
    }
}

impl<const SHARDS: usize, T, const OBSERVE: bool> OwnedBuffer<SHARDS, Vec<T>, OBSERVE> {
    pub(super) fn drain<R>(&mut self, range: R) -> std::vec::Drain<'_, T>
    where
        R: RangeBounds<usize>,
    {
        self.value.drain(range)
    }

    #[inline]
    pub(super) fn ensure_len(&mut self, min_len: usize) -> Result<(), PoolError>
    where
        T: Clone + Default,
    {
        if min_len <= self.value.len() {
            return Ok(());
        }
        self.grow(min_len)?;
        self.value.resize(min_len, T::default());
        Ok(())
    }

    pub(super) fn retain<F>(&mut self, keep: F)
    where
        F: FnMut(&T) -> bool,
    {
        self.value.retain(keep);
    }

    pub(super) fn shrink_to_fit(&mut self) {
        let len = self.value.len();
        self.core.shrink_to(&mut self.value, len);
    }

    pub(super) fn try_extend<I>(&mut self, values: I) -> Result<(), PoolError>
    where
        I: IntoIterator<Item = T>,
    {
        for value in values {
            self.try_push(value)?;
        }
        Ok(())
    }

    pub(super) fn try_extend_from_slice(&mut self, values: &[T]) -> Result<(), PoolError>
    where
        T: Clone,
    {
        let new_len = self.checked_extended_len(values.len())?;
        self.grow(new_len)?;
        self.value.extend_from_slice(values);
        Ok(())
    }

    pub(super) fn try_push(&mut self, value: T) -> Result<(), PoolError> {
        let new_len = self.checked_extended_len(1)?;
        self.grow(new_len)?;
        self.value.push(value);
        Ok(())
    }

    fn checked_extended_len(&self, additional: usize) -> Result<usize, PoolError> {
        self.value
            .len()
            .checked_add(additional)
            .ok_or(PoolError::CapacityOverflow {
                elements: usize::MAX,
                element_size: size_of::<T>(),
            })
    }

    delegate::delegate! {
        to self.value {
            pub(super) fn dedup(&mut self)
            where
                T: PartialEq;
            pub(super) fn truncate(&mut self, len: usize);
        }
    }
}

impl<const SHARDS: usize, const OBSERVE: bool> OwnedBuffer<SHARDS, String, OBSERVE> {
    pub(super) fn try_push_str(&mut self, content: &str) -> Result<(), PoolError> {
        let new_len =
            self.value
                .len()
                .checked_add(content.len())
                .ok_or(PoolError::CapacityOverflow {
                    elements: usize::MAX,
                    element_size: 1,
                })?;
        self.grow(new_len)?;
        self.value.push_str(content);
        Ok(())
    }
}

impl<const SHARDS: usize, B, const OBSERVE: bool> Drop for OwnedBuffer<SHARDS, B, OBSERVE>
where
    B: Storage,
{
    fn drop(&mut self) {
        self.core.put(mem::take(&mut self.value), self.shard_idx);
    }
}
