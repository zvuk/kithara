use crate::errors::{CoreError, CoreResult};
use url::Url;

pub fn canonicalize_for_asset(url: &Url) -> CoreResult<String> {
    let mut canonical = url.clone();

    // Remove fragment and query
    canonical.set_fragment(None);
    canonical.set_query(None);

    // Normalize scheme and host to lowercase
    let scheme = canonical.scheme();
    let scheme_lower = scheme.to_lowercase();
    if scheme != scheme_lower {
        let _ = canonical.set_scheme(&scheme_lower);
    }

    if let Some(host) = canonical.host_str() {
        let host_lower = host.to_lowercase();
        if host != host_lower {
            let _ = canonical.set_host(Some(&host_lower));
        }
    }

    // Remove default ports
    match (canonical.scheme(), canonical.port()) {
        ("https", Some(443)) | ("http", Some(80)) => {
            let _ = canonical.set_port(None);
        }
        _ => {}
    }

    canonical
        .to_string()
        .parse::<String>()
        .map_err(|e| CoreError::Canonicalization(e.to_string()))
}

pub fn canonicalize_for_resource(url: &Url) -> CoreResult<String> {
    let mut canonical = url.clone();

    // Remove fragment but keep query
    canonical.set_fragment(None);

    // Normalize scheme and host to lowercase
    let scheme = canonical.scheme();
    let scheme_lower = scheme.to_lowercase();
    if scheme != scheme_lower {
        let _ = canonical.set_scheme(&scheme_lower);
    }

    if let Some(host) = canonical.host_str() {
        let host_lower = host.to_lowercase();
        if host != host_lower {
            let _ = canonical.set_host(Some(&host_lower));
        }
    }

    // Remove default ports
    match (canonical.scheme(), canonical.port()) {
        ("https", Some(443)) | ("http", Some(80)) => {
            let _ = canonical.set_port(None);
        }
        _ => {}
    }

    canonical
        .to_string()
        .parse::<String>()
        .map_err(|e| CoreError::Canonicalization(e.to_string()))
}
