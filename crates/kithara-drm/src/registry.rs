use std::{collections::HashMap, fmt, sync::Arc};

use bytes::Bytes;
use url::Url;

/// Result of processing a key through a [`KeyProcessor`].
pub type KeyProcessResult = Result<Bytes, crate::DrmError>;

/// Callback that transforms raw key bytes fetched from the server.
pub type KeyProcessor = Arc<dyn Fn(Bytes) -> KeyProcessResult + Send + Sync>;

/// Pattern for matching key-URL domains.
#[derive(Clone, Debug)]
pub enum DomainMatcher {
    /// Exact domain match (e.g. `"zvuk.com"` matches only `zvuk.com`).
    Exact(String),
    /// Wildcard subdomain match (e.g. `"*.zvuk.com"` matches
    /// `cdn.zvuk.com`, `edge.cdn.zvuk.com`, but not `zvuk.com` itself).
    Wildcard(String),
}

impl DomainMatcher {
    /// Parse a pattern string into a [`DomainMatcher`].
    ///
    /// `"*.example.com"` → [`DomainMatcher::Wildcard`],
    /// `"example.com"` → [`DomainMatcher::Exact`].
    #[must_use]
    pub fn parse(pattern: &str) -> Self {
        let lower = pattern.to_ascii_lowercase();
        lower.strip_prefix("*.").map_or_else(
            || Self::Exact(lower.clone()),
            |suffix| Self::Wildcard(suffix.to_string()),
        )
    }

    fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match self {
            Self::Exact(domain) => host == *domain,
            Self::Wildcard(suffix) => {
                host.ends_with(suffix.as_str())
                    && host.len() > suffix.len()
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            }
        }
    }
}

/// A rule binding domain patterns to a key processor + optional headers.
#[derive(Clone)]
pub struct KeyProcessorRule {
    matchers: Vec<DomainMatcher>,
    processor: KeyProcessor,
    headers: Option<HashMap<String, String>>,
}

impl fmt::Debug for KeyProcessorRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyProcessorRule")
            .field("matchers", &self.matchers)
            .field("processor", &"<fn>")
            .field("headers", &self.headers)
            .finish()
    }
}

/// Registry of domain-scoped key processors.
///
/// When HLS fetches a DRM key from a URL, the registry is consulted to
/// find a matching processor. The first rule whose domain pattern matches
/// the key URL's host wins. Unmatched URLs use the raw key as-is.
#[derive(Clone, Default, Debug)]
pub struct KeyProcessorRegistry {
    rules: Vec<KeyProcessorRule>,
}

impl KeyProcessorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a rule: `patterns` is a list of domain patterns (see
    /// [`DomainMatcher::parse`]), `processor` transforms key bytes,
    /// `headers` are sent with the key request.
    pub fn add(
        &mut self,
        patterns: &[&str],
        processor: KeyProcessor,
        headers: Option<HashMap<String, String>>,
    ) -> &mut Self {
        self.rules.push(KeyProcessorRule {
            matchers: patterns.iter().map(|p| DomainMatcher::parse(p)).collect(),
            processor,
            headers,
        });
        self
    }

    /// Find a processor + headers for the given key URL.
    ///
    /// Returns `None` if no rule matches — caller should use the raw key.
    #[must_use]
    pub fn find(&self, key_url: &Url) -> Option<(&KeyProcessor, Option<&HashMap<String, String>>)> {
        let host = key_url.host_str()?;
        for rule in &self.rules {
            if rule.matchers.iter().any(|m| m.matches(host)) {
                return Some((&rule.processor, rule.headers.as_ref()));
            }
        }
        None
    }

    /// Whether the registry has any rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_processor() -> KeyProcessor {
        Arc::new(|key| Ok(key))
    }

    fn reverse_processor() -> KeyProcessor {
        Arc::new(|key| {
            let mut v = key.to_vec();
            v.reverse();
            Ok(Bytes::from(v))
        })
    }

    #[test]
    fn exact_match() {
        let m = DomainMatcher::parse("zvuk.com");
        assert!(m.matches("zvuk.com"));
        assert!(m.matches("ZVUK.COM"));
        assert!(!m.matches("cdn.zvuk.com"));
        assert!(!m.matches("nozvuk.com"));
    }

    #[test]
    fn wildcard_match() {
        let m = DomainMatcher::parse("*.zvuk.com");
        assert!(m.matches("cdn.zvuk.com"));
        assert!(m.matches("edge.cdn.zvuk.com"));
        assert!(!m.matches("zvuk.com"));
        assert!(!m.matches("nozvuk.com"));
    }

    #[test]
    fn wildcard_case_insensitive() {
        let m = DomainMatcher::parse("*.ZVQ.ME");
        assert!(m.matches("cdn-edge.zvq.me"));
        assert!(m.matches("CDN-EDGE.ZVQ.ME"));
    }

    #[test]
    fn registry_find_first_match() {
        let mut reg = KeyProcessorRegistry::new();
        reg.add(&["*.zvuk.com", "*.zvq.me"], noop_processor(), None);
        reg.add(&["other.com"], reverse_processor(), None);

        let url = Url::parse("https://cdn-edge.zvq.me/keys/track.key").expect("url");
        assert!(reg.find(&url).is_some());

        let url = Url::parse("https://other.com/key.bin").expect("url");
        assert!(reg.find(&url).is_some());

        let url = Url::parse("https://unknown.com/key.bin").expect("url");
        assert!(reg.find(&url).is_none());
    }

    #[test]
    fn registry_returns_per_rule_headers() {
        let mut reg = KeyProcessorRegistry::new();
        let mut headers = HashMap::new();
        headers.insert("X-Encrypted-Key".to_string(), "seed123".to_string());
        reg.add(&["*.zvuk.com"], noop_processor(), Some(headers));
        reg.add(&["silvercomet.top"], noop_processor(), None);

        let url = Url::parse("https://cdn.zvuk.com/key.bin").expect("url");
        let (_, hdrs) = reg.find(&url).expect("matched");
        assert!(hdrs.is_some());
        assert_eq!(hdrs.expect("headers")["X-Encrypted-Key"], "seed123");

        let url = Url::parse("https://silvercomet.top/key.bin").expect("url");
        let (_, hdrs) = reg.find(&url).expect("matched");
        assert!(hdrs.is_none());
    }

    #[test]
    fn registry_processor_transforms_key() {
        let mut reg = KeyProcessorRegistry::new();
        reg.add(&["example.com"], reverse_processor(), None);

        let url = Url::parse("https://example.com/key.bin").expect("url");
        let (proc, _) = reg.find(&url).expect("matched");
        let result = proc(Bytes::from_static(b"abcd")).expect("ok");
        assert_eq!(&result[..], b"dcba");
    }

    #[test]
    fn no_match_returns_none() {
        let reg = KeyProcessorRegistry::new();
        let url = Url::parse("https://example.com/key.bin").expect("url");
        assert!(reg.find(&url).is_none());
    }

    #[test]
    fn empty_registry() {
        let reg = KeyProcessorRegistry::new();
        assert!(reg.is_empty());
    }
}
