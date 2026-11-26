use crate::{PoolKey, region::PoolSlot};

/// Compile-time evidence that a schema contains `K`.
///
/// `#[derive(Pool)]` implements it once per `PoolSlot<K>` field and adds a
/// module-private builder that configures every field exactly once:
///
/// ```
/// use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolSlot};
///
/// #[derive(Pool)]
/// pub struct ExamplePools {
///     bytes: PoolSlot<u8>,
///     samples: PoolSlot<f32>,
/// }
///
/// let config = || PoolConfig::builder().max_buffers(32).build();
/// let pools = ExamplePools::builder(OverallBudget(1024))
///     .bytes(config())
///     .samples(config())
///     .build()
///     .unwrap();
/// assert_eq!(pools.get::<u8>().len(), 0);
/// assert_eq!(pools.get_with_len::<f32>(4).unwrap().len(), 4);
/// ```
pub trait HasPool<K>
where
    K: PoolKey,
{
    /// Return the generated opaque slot for static facade dispatch.
    #[doc(hidden)]
    fn __slot(&self) -> &PoolSlot<K>;
}
