use std::fmt;

use kithara_bufpool::{PooledOwned, Reuse, SharedPool};

const SHARDS: usize = 1;

#[derive(Debug)]
pub(super) struct DrawBuffer<T>(Vec<T>);

impl<T> Default for DrawBuffer<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> Reuse for DrawBuffer<T> {
    fn byte_size(&self) -> usize {
        self.0.capacity().saturating_mul(size_of::<T>())
    }

    fn reuse(&mut self, max_capacity: usize) -> bool {
        self.0.clear();
        self.0.capacity() > 0 && self.0.capacity() <= max_capacity
    }
}

pub(super) type VecPool<T> = SharedPool<SHARDS, DrawBuffer<T>>;
type VecGuard<T> = PooledOwned<SHARDS, DrawBuffer<T>>;

pub(super) enum Buffer<T> {
    Owned(Vec<T>),
    Pooled(VecGuard<T>),
}

impl<T> Buffer<T> {
    pub(super) const fn owned(values: Vec<T>) -> Self {
        Self::Owned(values)
    }

    pub(super) fn pooled(pool: &VecPool<T>) -> Self {
        Self::Pooled(pool.get())
    }

    pub(super) fn push(&mut self, value: T) {
        match self {
            Self::Owned(values) => values.push(value),
            Self::Pooled(guard) => guard.0.push(value),
        }
    }

    pub(super) fn as_slice(&self) -> &[T] {
        match self {
            Self::Owned(values) => values,
            Self::Pooled(guard) => &guard.0,
        }
    }

    pub(super) fn into_pooled(self, pool: &VecPool<T>) -> Self {
        match self {
            pooled @ Self::Pooled(_) => pooled,
            Self::Owned(mut values) => {
                let mut pooled = Self::pooled(pool);
                if let Self::Pooled(guard) = &mut pooled {
                    guard.0.append(&mut values);
                }
                pooled
            }
        }
    }
}

impl<T> Default for Buffer<T> {
    fn default() -> Self {
        Self::Owned(Vec::new())
    }
}

impl<T: Clone> Clone for Buffer<T> {
    fn clone(&self) -> Self {
        Self::Owned(self.as_slice().to_vec())
    }
}

impl<T: fmt::Debug> fmt::Debug for Buffer<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(formatter)
    }
}

impl<T: PartialEq> PartialEq for Buffer<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}
