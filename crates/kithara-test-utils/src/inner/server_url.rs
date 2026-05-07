use url::Url;

/// Join a relative test path onto a server base URL without dropping base path segments.
///
/// Unified test servers may expose a base URL with path segments already present.
/// `Url::join("/path")` would discard those existing segments, so test fixtures
/// normalize rooted paths before joining.
#[must_use]
pub fn join_server_url(base_url: &Url, path: &str) -> Url {
    let base = base_url.as_str().trim_end_matches('/');
    let path = path.trim_start_matches('/');
    Url::parse(&format!("{base}/{path}")).expect("join server URL path")
}
