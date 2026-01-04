use url::Url;

use crate::errors::{CoreError, CoreResult};

pub fn canonicalize_for_asset(url: &Url) -> CoreResult<String> {
    // Validate that URL has required components for asset identification
    if url.scheme().is_empty() {
        return Err(CoreError::MissingComponent("scheme".to_string()));
    }
    if url.host().is_none() {
        return Err(CoreError::MissingComponent("host".to_string()));
    }

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
    // Validate that URL has required components for resource identification
    if url.scheme().is_empty() {
        return Err(CoreError::MissingComponent("scheme".to_string()));
    }
    if url.host().is_none() {
        return Err(CoreError::MissingComponent("host".to_string()));
    }

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

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    #[test]
    fn canonicalize_for_asset_errors_on_missing_host() {
        let url = Url::parse("file:///path/to/audio.mp3").unwrap();

        let result = canonicalize_for_asset(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(CoreError::MissingComponent(host)) if host == "host"));
    }

    #[test]
    fn canonicalize_for_resource_errors_on_missing_host() {
        let url = Url::parse("file:///path/to/audio.mp3?token=123").unwrap();

        let result = canonicalize_for_resource(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(CoreError::MissingComponent(host)) if host == "host"));
    }
}
