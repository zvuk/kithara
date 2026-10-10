/// FFI representation of an asset whose resources share one cache root.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    all(feature = "uniffi", not(target_arch = "wasm32")),
    derive(uniffi::Enum)
)]
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
#[cfg_attr(
    all(feature = "uniffi", not(target_arch = "wasm32")),
    derive(uniffi::Enum)
)]
pub enum FfiAssetResource {
    /// Direct-file source bytes with their resolved extension.
    Source { extension: String },
    /// A URL-addressed resource such as a playlist, init, segment, or key.
    Url { url: String },
    /// A named derived artifact such as track analysis.
    Named { namespace: String, name: String },
}

/// Domain-scoped query parameters that identify remote media content.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    all(feature = "uniffi", not(target_arch = "wasm32")),
    derive(uniffi::Record)
)]
pub struct FfiCacheIdentityRule {
    /// Exact hosts, `*.domain` subdomain patterns, or `*`.
    pub domains: Vec<String>,
    /// Query parameter names included in the cache identity.
    pub query_parameters: Vec<String>,
}

/// Pure, deterministic cache-layout callbacks, fast, non-blocking, non-throwing
/// and safe on arbitrary background threads. Output must contain no query text,
/// credentials or other secrets.
/// `root` runs once per new store scope, `path` once per new resource key;
/// operations on that key do not repeat either callback. Invalid output fails
/// creation without rewriting it or falling back to the default layout.
/// `root` is one non-empty component other than `_index`; `path` is a non-empty
/// relative `/`-separated path with no component ending in `.tmp`.
/// Components are ASCII, at most 96 bytes, never `.` or `..`, never end in dot
/// or space, and contain no controls or `< > : " / \ | ? *`.
/// Windows device names are refused; `_index`, `.tmp` and device-name checks
/// are case-insensitive.
#[kithara::mock(api = FfiAssetLayoutMock)]
#[cfg_attr(
    all(feature = "uniffi", not(target_arch = "wasm32")),
    uniffi::export(with_foreign)
)]
pub trait FfiAssetLayout: Send + Sync {
    fn path(&self, resource: FfiAssetResource) -> String;
    fn root(&self, source: FfiAssetSource) -> String;
}
