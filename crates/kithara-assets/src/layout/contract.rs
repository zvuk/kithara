use std::fmt;

use super::{AssetResource, AssetSource};

/// Policy that owns every cache-relative asset and resource path.
pub trait AssetLayout: fmt::Debug + Send + Sync {
    /// Return the single cache-root component for `source`.
    fn root(&self, source: &AssetSource) -> String;

    /// Return the relative path for `resource` inside an asset root.
    fn path(&self, resource: &AssetResource) -> String;
}
