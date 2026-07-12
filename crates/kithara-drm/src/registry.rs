use std::{collections::HashMap, fmt};

use bon::Builder;
use bytes::Bytes;
use kithara_platform::sync::Arc;
use url::Url;

/// Result of processing a key through a [`KeyProcessor`].
pub type KeyProcessResult = Result<Bytes, crate::DrmError>;

/// Callback that transforms raw key bytes fetched from the server.
pub type KeyProcessor = Arc<dyn Fn(Bytes) -> KeyProcessResult + Send + Sync>;

/// Factory that produces a fresh [`KeyRequest`] for every key fetch.
/// Called by [`KeyProcessorRegistry`] consumers on each request so
/// per-request material (the X-Encrypted-Key salt and the cipher
/// keyed on it) never gets reused across keys.
pub type KeyRequestFactory = Arc<dyn Fn() -> KeyRequest + Send + Sync>;

/// Per-request state for a single key fetch: the headers to send
/// (typically a freshly-generated `X-Encrypted-Key` salt) and the
/// matching processor that will decrypt the response body with the
/// same salt. Built by [`KeyRequestFactory`].
#[non_exhaustive]
pub struct KeyRequest {
    /// Headers added to the outgoing key request. Merged on top of
    /// [`KeyProcessorRule::headers`] (per-request entries win on key
    /// collision).
    pub headers: HashMap<String, String>,
    /// Processor that decrypts the response body using state derived
    /// from this request's headers (e.g. the salt embedded above).
    pub processor: KeyProcessor,
}

impl KeyRequest {
    /// Construct a fresh request pair. Required because the struct
    /// is `#[non_exhaustive]` (so future fields can be added without
    /// breaking external factories).
    #[must_use]
    pub fn new(headers: HashMap<String, String>, processor: KeyProcessor) -> Self {
        Self { headers, processor }
    }
}

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

/// A rule binding domain patterns to a key-request factory +
/// per-provider request shape (static base headers, query params).
///
/// The factory produces a fresh [`KeyRequest`] (per-request salt +
/// matching processor) on every key fetch — see [`KeyRequestFactory`].
/// `headers` is the rule's static base shape (UA, X-App-*, X-Auth-Token,
/// Referer); per-request headers (X-Encrypted-Key) come from the factory.
///
/// Build with [`KeyProcessorRule::for_domains`] then chain
/// bon-generated `headers(...)` / `query_params(...)` setters and
/// `.build()`.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct KeyProcessorRule {
    /// Static headers (UA, auth, X-App-*) merged on top of base
    /// downloader headers for every fetch this rule matches. The
    /// per-request `X-Encrypted-Key` salt is **not** part of this
    /// map — it comes from [`Self::key_request_factory`] on each
    /// individual key request.
    pub headers: Option<HashMap<String, String>>,
    /// Query parameters appended to key URLs that match this rule.
    pub query_params: Option<HashMap<String, String>>,
    key_request_factory: KeyRequestFactory,
    matchers: Vec<DomainMatcher>,
}

impl fmt::Debug for KeyProcessorRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyProcessorRule")
            .field("headers", &self.headers)
            .field("query_params", &self.query_params)
            .field("key_request_factory", &"<fn>")
            .field("matchers", &self.matchers)
            .finish()
    }
}

impl KeyProcessorRule {
    /// Rule bound to `patterns` (parsed via [`DomainMatcher::parse`])
    /// and the given `factory`. Headers and query params default to
    /// `None`; for those use [`KeyProcessorRule::for_domains`].
    #[must_use]
    pub fn new<P, I>(patterns: I, factory: KeyRequestFactory) -> Self
    where
        P: AsRef<str>,
        I: IntoIterator<Item = P>,
    {
        Self::for_domains(patterns, factory).build()
    }

    /// Produce a fresh [`KeyRequest`] for one key fetch. Caller must
    /// invoke this once per outgoing request — never cache or reuse
    /// the returned headers / processor across fetches.
    #[must_use]
    pub fn build_request(&self) -> KeyRequest {
        (self.key_request_factory)()
    }

    /// Chainable counterpart to [`KeyProcessorRule::new`]: returns a
    /// builder with `factory` and `matchers` already set so callers
    /// can attach `.headers(...)` / `.query_params(...)` then `.build()`.
    pub fn for_domains<P, I>(
        patterns: I,
        factory: KeyRequestFactory,
    ) -> KeyProcessorRuleBuilder<
        key_processor_rule_builder::SetMatchers<key_processor_rule_builder::SetKeyRequestFactory>,
    >
    where
        P: AsRef<str>,
        I: IntoIterator<Item = P>,
    {
        Self::builder().key_request_factory(factory).matchers(
            patterns
                .into_iter()
                .map(|p| DomainMatcher::parse(p.as_ref()))
                .collect(),
        )
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
    // ast-grep-ignore: style.prefer-default-derive
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
