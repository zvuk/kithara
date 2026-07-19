use std::fmt;

use super::{AssetResource, AssetSource};

/// Policy that owns every cache-relative asset and resource path.
///
/// Implementations must be deterministic, non-blocking, and secret-free.
pub trait AssetLayout: fmt::Debug + Send + Sync {
    /// Return the single cache-root component for `source`.
    ///
    /// Called once by each [`AssetStore::scope`](crate::AssetStore::scope) invocation.
    fn root(&self, source: &AssetSource) -> String;

    /// Return the relative path for `resource` inside an asset root.
    ///
    /// Called once by each [`AssetScope::key`](super::AssetScope::key) invocation.
    /// Store operations on the resulting key do not consult the layout again.
    fn path(&self, resource: &AssetResource) -> String;
}
