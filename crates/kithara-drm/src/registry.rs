use std::{collections::HashMap, fmt, sync::Arc};

use bytes::Bytes;
use derivative::Derivative;
use derive_setters::Setters;
use url::Url;

/// Result of processing a key through a [`KeyProcessor`].
pub type KeyProcessResult = Result<Bytes, crate::DrmError>;

/// Callback that transforms raw key bytes fetched from the server.
pub type KeyProcessor = Arc<dyn Fn(Bytes) -> KeyProcessResult + Send + Sync>;

/// Pattern for matching key-URL domains.
#[derive(Clone, Debug)]
#[non_exhaustive]
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

/// A rule binding domain patterns to a key processor + per-provider
/// request shape (headers, query params).
///
/// Build with [`KeyProcessorRule::new`] + `.with_headers(...)` /
/// `.with_query_params(...)` setters.
#[derive(Clone, Derivative, Setters)]
#[derivative(Debug)]
#[setters(prefix = "with_", strip_option)]
#[non_exhaustive]
pub struct KeyProcessorRule {
    #[setters(skip)]
    matchers: Vec<DomainMatcher>,
    #[setters(skip)]
    #[derivative(Debug(format_with = "fmt_processor"))]
    processor: KeyProcessor,
    /// Headers appended to key requests that match this rule.
    pub headers: Option<HashMap<String, String>>,
    /// Query parameters appended to key URLs that match this rule.
    pub query_params: Option<HashMap<String, String>>,
}

fn fmt_processor(_: &KeyProcessor, f: &mut fmt::Formatter) -> fmt::Result {
    f.write_str("<fn>")
}

impl KeyProcessorRule {
    /// Create a rule bound to `patterns` (parsed via
    /// [`DomainMatcher::parse`]) with the given `processor`. Headers
    /// and query params default to `None`.
    #[must_use]
    pub fn new<P, I>(patterns: I, processor: KeyProcessor) -> Self
    where
        P: AsRef<str>,
        I: IntoIterator<Item = P>,
    {
        Self {
            matchers: patterns
                .into_iter()
                .map(|p| DomainMatcher::parse(p.as_ref()))
                .collect(),
            processor,
            headers: None,
            query_params: None,
        }
    }

    #[must_use]
    pub fn processor(&self) -> &KeyProcessor {
        &self.processor
    }
}

/// Registry of domain-scoped key processors.
///
/// When HLS fetches a DRM key from a URL, the registry is consulted to
/// find a matching rule. The first rule whose domain pattern matches
/// the key URL's host wins. Unmatched URLs use the raw key as-is.
#[derive(Clone, Default, Debug)]
#[non_exhaustive]
pub struct KeyProcessorRegistry {
    rules: Vec<KeyProcessorRule>,
}

impl KeyProcessorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a [`KeyProcessorRule`] to the registry.
    pub fn add(&mut self, rule: KeyProcessorRule) -> &mut Self {
        self.rules.push(rule);
        self
    }

    /// Find the first rule matching `key_url` by host.
    ///
    /// Returns `None` if no rule matches — caller should use the raw
    /// key, no extra headers, no extra query params.
    #[must_use]
    pub fn find(&self, key_url: &Url) -> Option<&KeyProcessorRule> {
        let host = key_url.host_str()?;
        self.rules
            .iter()
            .find(|rule| rule.matchers.iter().any(|m| m.matches(host)))
    }

    /// Whether the registry has any rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

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

    #[kithara::test]
    fn exact_match() {
        let m = DomainMatcher::parse("zvuk.com");
        assert!(m.matches("zvuk.com"));
        assert!(m.matches("ZVUK.COM"));
        assert!(!m.matches("cdn.zvuk.com"));
        assert!(!m.matches("nozvuk.com"));
    }

    #[kithara::test]
    fn wildcard_match() {
        let m = DomainMatcher::parse("*.zvuk.com");
        assert!(m.matches("cdn.zvuk.com"));
        assert!(m.matches("edge.cdn.zvuk.com"));
        assert!(!m.matches("zvuk.com"));
        assert!(!m.matches("nozvuk.com"));
    }

    #[kithara::test]
    fn wildcard_case_insensitive() {
        let m = DomainMatcher::parse("*.ZVQ.ME");
        assert!(m.matches("cdn-edge.zvq.me"));
        assert!(m.matches("CDN-EDGE.ZVQ.ME"));
    }

    #[kithara::test]
    fn rule_builder_sets_headers_and_query_params() {
        let mut headers = HashMap::new();
        headers.insert("X-Encrypted-Key".to_string(), "seed123".to_string());
        let mut params = HashMap::new();
        params.insert("token".to_string(), "abc".to_string());

        let rule = KeyProcessorRule::new(["*.zvuk.com"], noop_processor())
            .with_headers(headers.clone())
            .with_query_params(params.clone());

        assert_eq!(rule.headers.as_ref().expect("headers"), &headers);
        assert_eq!(rule.query_params.as_ref().expect("params"), &params);
    }

    #[kithara::test]
    fn registry_find_first_match() {
        let mut reg = KeyProcessorRegistry::new();
        reg.add(KeyProcessorRule::new(
            ["*.zvuk.com", "*.zvq.me"],
            noop_processor(),
        ));
        reg.add(KeyProcessorRule::new(["other.com"], reverse_processor()));

        let url = Url::parse("https://cdn-edge.zvq.me/keys/track.key").expect("url");
        assert!(reg.find(&url).is_some());

        let url = Url::parse("https://other.com/key.bin").expect("url");
        assert!(reg.find(&url).is_some());

        let url = Url::parse("https://unknown.com/key.bin").expect("url");
        assert!(reg.find(&url).is_none());
    }

    #[kithara::test]
    fn registry_returns_per_rule_headers() {
        let mut reg = KeyProcessorRegistry::new();
        let mut headers = HashMap::new();
        headers.insert("X-Encrypted-Key".to_string(), "seed123".to_string());
        reg.add(
            KeyProcessorRule::new(["*.zvuk.com"], noop_processor()).with_headers(headers.clone()),
        );
        reg.add(KeyProcessorRule::new(["silvercomet.top"], noop_processor()));

        let url = Url::parse("https://cdn.zvuk.com/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        assert_eq!(
            rule.headers.as_ref().expect("headers")["X-Encrypted-Key"],
            "seed123"
        );

        let url = Url::parse("https://silvercomet.top/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        assert!(rule.headers.is_none());
    }

    #[kithara::test]
    fn registry_returns_per_rule_query_params() {
        let mut reg = KeyProcessorRegistry::new();
        let mut params = HashMap::new();
        params.insert("token".to_string(), "xyz".to_string());
        reg.add(
            KeyProcessorRule::new(["*.zvuk.com"], noop_processor())
                .with_query_params(params.clone()),
        );
        reg.add(KeyProcessorRule::new(["silvercomet.top"], noop_processor()));

        let url = Url::parse("https://cdn.zvuk.com/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        assert_eq!(rule.query_params.as_ref().expect("params")["token"], "xyz");

        let url = Url::parse("https://silvercomet.top/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        assert!(rule.query_params.is_none());
    }

    #[kithara::test]
    fn registry_processor_transforms_key() {
        let mut reg = KeyProcessorRegistry::new();
        reg.add(KeyProcessorRule::new(["example.com"], reverse_processor()));

        let url = Url::parse("https://example.com/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        let result = rule.processor()(Bytes::from_static(b"abcd")).expect("ok");
        assert_eq!(&result[..], b"dcba");
    }

    #[kithara::test]
    fn no_match_returns_none() {
        let reg = KeyProcessorRegistry::new();
        let url = Url::parse("https://example.com/key.bin").expect("url");
        assert!(reg.find(&url).is_none());
    }

    #[kithara::test]
    fn empty_registry() {
        let reg = KeyProcessorRegistry::new();
        assert!(reg.is_empty());
    }
}
