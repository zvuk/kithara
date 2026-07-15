use std::fmt;

use url::Url;

use super::asset_root_for_url;

/// On-disk layout policy carried by an [`AssetScope`](crate::AssetScope):
/// maps a resource URL to a relative path inside the asset's directory.
pub trait AssetLayout: fmt::Debug + Send + Sync {
    /// Derive the on-disk asset directory name for a source URL.
    #[must_use]
    fn asset_root(&self, url: &Url, name: Option<&str>) -> String {
        asset_root_for_url(url, name)
    }

    /// Derive the relative path for the resource at `url` inside the scope's asset root.
    #[must_use]
    fn rel_path(&self, url: &Url) -> String;
}
