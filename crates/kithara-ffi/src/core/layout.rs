/// FFI representation of an asset whose resources share one cache root.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiAssetSource {
    Remote {
        url: String,
        discriminator: Option<String>,
    },
    Local {
        path: String,
    },
}

/// FFI representation of one resource within an asset.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiAssetResource {
    /// Direct-file source bytes with their resolved extension.
    Source { extension: String },
    /// A URL-addressed resource such as a playlist, init, segment, or key.
    Url { url: String },
    /// A named derived artifact such as track analysis.
    Named { namespace: String, name: String },
}

/// Foreign cache layout callback.
///
/// Implementations must be pure and deterministic, fast, non-blocking,
/// non-throwing, and safe to call from arbitrary background threads. Returned
/// values must not contain query text, credentials, or other secrets.
/// `root` is called once for each store scope being created. `path` is called
/// once for each resource key being minted. Cache operations using that key do
/// not invoke either callback again.
/// Invalid output fails scope or key creation and never falls back to the
/// default layout.
///
/// `root` must return exactly one non-empty component and cannot equal
/// `_index`. `path` must return a non-empty relative path of components
/// separated by `/`; no component may end in `.tmp`. Components are ASCII,
/// at most 96 bytes, never `.` or `..`, do not end in a dot or space, are not
/// Windows device names, and contain neither control bytes nor
/// `< > : " / \ | ? *`. Comparisons for `_index`, `.tmp`, and device names are
/// case-insensitive. The store rejects invalid output instead of rewriting it.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait FfiAssetLayout: Send + Sync {
    fn root(&self, source: FfiAssetSource) -> String;
    fn path(&self, resource: FfiAssetResource) -> String;
}
