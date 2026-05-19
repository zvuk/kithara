use std::{collections::HashMap, fmt, sync::Arc};

use bon::Builder;
use bytes::Bytes;
use url::Url;

/// Result of processing a key through a [`KeyProcessor`].
pub type KeyProcessResult = Result<Bytes, crate::DrmError>;

/// Callback that transforms raw key bytes fetched from the server.
pub type KeyProcessor = Arc<dyn Fn(Bytes) -> KeyProcessResult + Send + Sync>;

/// Pattern for matching key-URL domains.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DomainMatcher {
    /// Match-any-host pattern (`"*"`). Place rules with this matcher
    /// LAST in the registry — they would otherwise mask any specific
    /// rule registered after them.
    All,
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
            Self::All => true,
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
    /// `"*"` → [`DomainMatcher::All`],
    /// `"*.example.com"` → [`DomainMatcher::Wildcard`],
    /// `"example.com"` → [`DomainMatcher::Exact`].
    #[must_use]
    pub fn parse(pattern: &str) -> Self {
        let lower = pattern.to_ascii_lowercase();
        if lower == "*" {
            return Self::All;
        }
        lower.strip_prefix("*.").map_or_else(
            || Self::Exact(lower.clone()),
            |suffix| Self::Wildcard(suffix.to_string()),
        )
    }
}

/// A rule binding domain patterns to a key processor + per-provider
/// request shape (headers, query params).
///
/// Build with [`KeyProcessorRule::new`] then chain bon-generated
/// `headers(...)` / `query_params(...)` setters and `.build()`.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct KeyProcessorRule {
    /// Headers appended to key requests that match this rule.
    pub headers: Option<HashMap<String, String>>,
    /// Query parameters appended to key URLs that match this rule.
    pub query_params: Option<HashMap<String, String>>,
    processor: KeyProcessor,
    matchers: Vec<DomainMatcher>,
}

impl fmt::Debug for KeyProcessorRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyProcessorRule")
            .field("headers", &self.headers)
            .field("query_params", &self.query_params)
            .field("processor", &"<fn>")
            .field("matchers", &self.matchers)
            .finish()
    }
}

impl KeyProcessorRule {
    /// Rule bound to `patterns` (parsed via [`DomainMatcher::parse`])
    /// and the given `processor`. Headers and query params default to
    /// `None`; for those use [`KeyProcessorRule::for_domains`].
    #[must_use]
    pub fn new<P, I>(patterns: I, processor: KeyProcessor) -> Self
    where
        P: AsRef<str>,
        I: IntoIterator<Item = P>,
    {
        Self::for_domains(patterns, processor).build()
    }

    /// Chainable counterpart to [`KeyProcessorRule::new`]: returns a
    /// builder with `processor` and `matchers` already set so callers
    /// can attach `.headers(...)` / `.query_params(...)` then `.build()`.
    pub fn for_domains<P, I>(
        patterns: I,
        processor: KeyProcessor,
    ) -> KeyProcessorRuleBuilder<
        key_processor_rule_builder::SetMatchers<key_processor_rule_builder::SetProcessor>,
    >
    where
        P: AsRef<str>,
        I: IntoIterator<Item = P>,
    {
        Self::builder().processor(processor).matchers(
            patterns
                .into_iter()
                .map(|p| DomainMatcher::parse(p.as_ref()))
                .collect(),
        )
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
