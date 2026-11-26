/// ```compile_fail
/// use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolSlot};
/// #[derive(Pool)]
/// pub struct MissingPools { bytes: PoolSlot<u8>, samples: PoolSlot<f32> }
/// let config = PoolConfig::builder().max_buffers(8).build();
/// let _ = MissingPools::builder(OverallBudget(64)).bytes(config).build();
/// ```
mod missing_registration {}

/// ```compile_fail
/// use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolSlot};
/// #[derive(Pool)]
/// pub struct DuplicatePools { bytes: PoolSlot<u8> }
/// let config = || PoolConfig::builder().max_buffers(8).build();
/// let _ = DuplicatePools::builder(OverallBudget(64))
///     .bytes(config())
///     .bytes(config());
/// ```
mod duplicate_registration {}

/// ```compile_fail
/// use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolSlot};
/// #[derive(Pool)]
/// pub struct KnownPools { bytes: PoolSlot<u8> }
/// let config = PoolConfig::builder().max_buffers(8).build();
/// let _ = KnownPools::builder(OverallBudget(64)).samples(config);
/// ```
mod unknown_registration {}

/// ```compile_fail
/// use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolSlot};
/// #[derive(Pool)]
/// pub struct BytePools { bytes: PoolSlot<u8> }
/// let pools = BytePools::builder(OverallBudget(64))
///     .bytes(PoolConfig::builder().max_buffers(8).build())
///     .build()
///     .unwrap();
/// let _ = pools.get::<f32>();
/// ```
mod unregistered_key {}

/// ```compile_fail
/// use kithara_bufpool::{Pool, PoolSlot};
/// type Bytes = u8;
/// type Octets = u8;
/// #[derive(Pool)]
/// pub struct AliasedPools { bytes: PoolSlot<Bytes>, octets: PoolSlot<Octets> }
/// ```
mod duplicate_key_through_aliases {}

/// ```compile_fail
/// use kithara_bufpool::Pool;
/// #[derive(Pool)]
/// pub struct ScalarPools { bytes: u8 }
/// ```
mod scalar_is_not_slot_storage {}

/// ```compile_fail
/// mod owner {
///     use kithara_bufpool::{Pool, PoolSlot};
///     #[derive(Pool)]
///     pub struct OwnedPools { bytes: PoolSlot<u8> }
/// }
/// mod sibling {
///     use kithara_bufpool::{OverallBudget, PoolConfig};
///     fn build() {
///         let config = PoolConfig::builder().max_buffers(8).build();
///         let _ = super::owner::OwnedPools::builder(OverallBudget(64)).bytes(config).build();
///     }
/// }
/// ```
mod builder_stays_with_the_declaring_module {}

/// ```
/// mod owner {
///     use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolError, PoolRegion, PoolSlot};
///     #[derive(Pool)]
///     pub struct OwnedPools { bytes: PoolSlot<u8> }
///     pub fn region() -> Result<PoolRegion<OwnedPools>, PoolError> {
///         let config = PoolConfig::builder().max_buffers(32).build();
///         OwnedPools::builder(OverallBudget(1024)).bytes(config).build()
///     }
/// }
/// mod sibling {
///     use kithara_bufpool::{HasPool, PoolRegion};
///     pub fn acquire<S: HasPool<u8>>(pools: &PoolRegion<S>) -> usize {
///         pools.get_with_len::<u8>(4).unwrap().len()
///     }
/// }
/// let pools = owner::region().unwrap();
/// assert_eq!(sibling::acquire(&pools), 4);
/// ```
mod sibling_uses_an_existing_region {}
