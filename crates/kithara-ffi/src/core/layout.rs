/// Foreign layout callback: maps a resource URL to a relative path inside the asset's
/// cache directory. `rel_path` must be pure (same URL -> same path), never block or throw,
/// and runs on arbitrary background threads; hostile or empty returns are rejected
/// store-side with a typed error, never silently rewritten.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait FfiAssetLayout: Send + Sync {
    fn rel_path(&self, url: String) -> String;
}
