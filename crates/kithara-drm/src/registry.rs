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
    pub(crate) fn matches(&self, host: &str) -> bool {
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
    /// Headers appended to key requests that match this rule.
    pub headers: Option<HashMap<String, String>>,
    /// Query parameters appended to key URLs that match this rule.
    pub query_params: Option<HashMap<String, String>>,
    #[setters(skip)]
    #[derivative(Debug(format_with = "fmt_processor"))]
    processor: KeyProcessor,
    #[setters(skip)]
    matchers: Vec<DomainMatcher>,
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
            processor,
            matchers: patterns
                .into_iter()
                .map(|p| DomainMatcher::parse(p.as_ref()))
                .collect(),
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
