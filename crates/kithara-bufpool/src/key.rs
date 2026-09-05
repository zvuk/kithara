use std::marker::PhantomData;

use crate::{
    ByteBuffer, PoolConfig, PoolError, PoolStats, PooledString, PooledVec, SampleBuffer,
    pool::Core, region::BuildContext,
};

mod sealed {
    pub trait Sealed {}
}

/// Unforgeable capability for crate-owned key operations.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct PoolAccess {
    _private: AccessToken,
}

#[derive(Clone, Copy)]
struct AccessToken;

impl PoolAccess {
    pub(crate) const fn new() -> Self {
        Self {
            _private: AccessToken,
        }
    }
}

/// A compile-time key for one physical pool in a closed schema.
pub trait PoolKey: sealed::Sealed {
    /// Element stored in this pool's buffers.
    type Item;
    /// Nominal checked guard returned by the facade.
    type Buffer;
    /// Opaque core used by schema-generated plumbing.
    #[doc(hidden)]
    type Core;

    /// Build one physical slot.
    #[doc(hidden)]
    fn __build(
        context: &BuildContext,
        config: PoolConfig,
        access: PoolAccess,
    ) -> Result<Self::Core, PoolError>;

    /// Acquire an empty buffer from the slot.
    #[doc(hidden)]
    fn __get(core: &Self::Core, access: PoolAccess) -> Self::Buffer;

    /// Snapshot reuse counters for this slot.
    #[doc(hidden)]
    fn __stats(core: &Self::Core, access: PoolAccess) -> PoolStats;
}

/// A pool key whose guard can be initialized to a requested element length.
pub trait PoolKeyWithLen: PoolKey {
    /// Acquire a buffer and grow it to `len` elements.
    #[doc(hidden)]
    fn __get_with_len(
        core: &Self::Core,
        len: usize,
        access: PoolAccess,
    ) -> Result<Self::Buffer, PoolError>;
}

/// A distinct physical pool that reuses another key's element and guard policy.
pub struct PoolAlias<Tag, K>(PhantomData<Tag>, PhantomData<K>);

impl<Tag, K> sealed::Sealed for PoolAlias<Tag, K> where K: PoolKey {}

impl<Tag, K> PoolKey for PoolAlias<Tag, K>
where
    K: PoolKey,
{
    type Buffer = K::Buffer;
    type Core = K::Core;
    type Item = K::Item;

    fn __build(
        context: &BuildContext,
        config: PoolConfig,
        access: PoolAccess,
    ) -> Result<Self::Core, PoolError> {
        K::__build(context, config, access)
    }

    fn __get(core: &Self::Core, access: PoolAccess) -> Self::Buffer {
        K::__get(core, access)
    }

    fn __stats(core: &Self::Core, access: PoolAccess) -> PoolStats {
        K::__stats(core, access)
    }
}

impl<Tag, K> PoolKeyWithLen for PoolAlias<Tag, K>
where
    K: PoolKeyWithLen,
{
    fn __get_with_len(
        core: &Self::Core,
        len: usize,
        access: PoolAccess,
    ) -> Result<Self::Buffer, PoolError> {
        K::__get_with_len(core, len, access)
    }
}

/// A checked vector pool key with an explicit shard count.
pub struct VecKey<T, const SHARDS: usize>(PhantomData<T>);

impl<T, const SHARDS: usize> sealed::Sealed for VecKey<T, SHARDS> {}

/// Opaque core for a registered vector key.
#[doc(hidden)]
pub struct VecCore<T, const SHARDS: usize>(kithara_platform::sync::Arc<Core<SHARDS, Vec<T>, true>>);

impl<T, const SHARDS: usize> PoolKey for VecKey<T, SHARDS>
where
    T: Send + 'static,
{
    type Buffer = PooledVec<T, SHARDS>;
    type Core = VecCore<T, SHARDS>;
    type Item = T;

    fn __build(
        context: &BuildContext,
        config: PoolConfig,
        _access: PoolAccess,
    ) -> Result<Self::Core, PoolError> {
        let limit = context.pool_limit(config.max_share)?;
        context
            .core::<SHARDS, Vec<T>, true>(config, limit)
            .map(VecCore)
    }

    fn __get(core: &Self::Core, _access: PoolAccess) -> Self::Buffer {
        PooledVec::new(core.0.acquire())
    }

    fn __stats(core: &Self::Core, _access: PoolAccess) -> PoolStats {
        core.0.stats()
    }
}

impl<T, const SHARDS: usize> PoolKeyWithLen for VecKey<T, SHARDS>
where
    T: Clone + Default + Send + 'static,
{
    fn __get_with_len(
        core: &Self::Core,
        len: usize,
        access: PoolAccess,
    ) -> Result<Self::Buffer, PoolError> {
        let mut buffer = Self::__get(core, access);
        buffer.ensure_len(len)?;
        Ok(buffer)
    }
}

/// A checked UTF-8 string pool key with an explicit shard count.
pub struct StringKey<const SHARDS: usize>;

impl<const SHARDS: usize> sealed::Sealed for StringKey<SHARDS> {}

/// Opaque core for a registered string key.
#[doc(hidden)]
pub struct StringCore<const SHARDS: usize>(kithara_platform::sync::Arc<Core<SHARDS, String, true>>);

impl<const SHARDS: usize> PoolKey for StringKey<SHARDS> {
    type Buffer = PooledString<SHARDS>;
    type Core = StringCore<SHARDS>;
    type Item = String;

    fn __build(
        context: &BuildContext,
        config: PoolConfig,
        _access: PoolAccess,
    ) -> Result<Self::Core, PoolError> {
        let limit = context.pool_limit(config.max_share)?;
        context
            .core::<SHARDS, String, true>(config, limit)
            .map(StringCore)
    }

    fn __get(core: &Self::Core, _access: PoolAccess) -> Self::Buffer {
        PooledString::new(core.0.acquire())
    }

    fn __stats(core: &Self::Core, _access: PoolAccess) -> PoolStats {
        core.0.stats()
    }
}

/// Opaque core for the built-in byte key.
#[doc(hidden)]
pub struct ByteCore(kithara_platform::sync::Arc<Core<32, Vec<u8>, false>>);

/// Opaque core for the built-in decoded-sample key.
#[doc(hidden)]
pub struct SampleCore(kithara_platform::sync::Arc<Core<8, Vec<f32>, false>>);

impl sealed::Sealed for u8 {}

impl PoolKey for u8 {
    type Buffer = ByteBuffer;
    type Core = ByteCore;
    type Item = Self;

    fn __build(
        context: &BuildContext,
        config: PoolConfig,
        _access: PoolAccess,
    ) -> Result<Self::Core, PoolError> {
        let limit = context.pool_limit(config.max_share)?;
        context
            .core::<32, Vec<Self>, false>(config, limit)
            .map(ByteCore)
    }

    fn __get(core: &Self::Core, _access: PoolAccess) -> Self::Buffer {
        ByteBuffer::new(core.0.acquire())
    }

    fn __stats(core: &Self::Core, _access: PoolAccess) -> PoolStats {
        core.0.stats()
    }
}

impl PoolKeyWithLen for u8 {
    fn __get_with_len(
        core: &Self::Core,
        len: usize,
        access: PoolAccess,
    ) -> Result<Self::Buffer, PoolError> {
        let mut buffer = Self::__get(core, access);
        buffer.ensure_len(len)?;
        Ok(buffer)
    }
}

impl sealed::Sealed for f32 {}

impl PoolKey for f32 {
    type Buffer = SampleBuffer;
    type Core = SampleCore;
    type Item = Self;

    fn __build(
        context: &BuildContext,
        config: PoolConfig,
        _access: PoolAccess,
    ) -> Result<Self::Core, PoolError> {
        let limit = context.pool_limit(config.max_share)?;
        context
            .core::<8, Vec<Self>, false>(config, limit)
            .map(SampleCore)
    }

    fn __get(core: &Self::Core, _access: PoolAccess) -> Self::Buffer {
        SampleBuffer::new(core.0.acquire())
    }

    fn __stats(core: &Self::Core, _access: PoolAccess) -> PoolStats {
        core.0.stats()
    }
}

impl PoolKeyWithLen for f32 {
    #[inline]
    fn __get_with_len(
        core: &Self::Core,
        len: usize,
        access: PoolAccess,
    ) -> Result<Self::Buffer, PoolError> {
        let mut buffer = Self::__get(core, access);
        buffer.ensure_len(len)?;
        Ok(buffer)
    }
}
