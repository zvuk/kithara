use url::Url;

/// Join a relative test path onto a server base URL without dropping base path segments.
///
/// Session-backed fixture servers use base URLs like `http://127.0.0.1:3333/s/{id}`.
/// `Url::join("/path")` would discard `/s/{id}`, so test fixtures must normalize
/// rooted paths before joining.
#[must_use]
pub fn join_server_url(base_url: &Url, path: &str) -> Url {
    let base = base_url.as_str().trim_end_matches('/');
    let path = path.trim_start_matches('/');
    Url::parse(&format!("{base}/{path}")).expect("join server URL path")
}
