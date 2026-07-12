use std::{cmp::min, collections::HashMap, fmt};

use bitflags::bitflags;
use bon::Builder;
use kithara_bufpool::BytePool;
use kithara_platform::{time::Duration, traits::FromWithParams};

bitflags! {
    /// HTTP `Accept-Encoding` algorithms the client advertises and is
    /// willing to decode. Reqwest auto-adds the corresponding
    /// `Accept-Encoding` header for every algorithm whose flag is set;
    /// the rest are disabled via `ClientBuilder::no_*` so the wire
    /// header stays in lockstep with this set.
    ///
    /// Subset selection matters when talking to anti-bot WAFs that
    /// fingerprint clients by their exact `Accept-Encoding` value.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Compression: u8 {
        const GZIP    = 1 << 0;
        const DEFLATE = 1 << 1;
        const BROTLI  = 1 << 2;
        const ZSTD    = 1 << 3;
    }
}

/// TLS+HTTP fingerprint the native `client-wreq` backend impersonates. The
/// DRM keyserver sits behind an anti-bot WAF that fingerprints the TLS
/// `ClientHello` (JA3) and 418-rejects non-browser stacks; presenting a real
/// browser fingerprint via `wreq` is what gets a 200. Default `Safari` matches
/// iOS `URLSession`; Android selects a different preset. Inert under the
/// `client-reqwest` backend and on wasm32 (no emulation; the browser fetch
/// already carries a real fingerprint).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImpersonatePreset {
    #[default]
    Safari,
    Chrome,
}

#[derive(Clone, Debug, Default, derive_more::From, PartialEq, Eq)]
pub struct Headers {
    #[from]
    inner: HashMap<String, String>,
}

impl Headers {
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(String::as_str)
    }

    pub fn insert<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
        self.inner.insert(key.into(), value.into());
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeSpec {
    pub end: Option<u64>,
    pub start: u64,
}

impl RangeSpec {
    #[must_use]
    pub fn new(start: u64, end: Option<u64>) -> Self {
        Self { end, start }
    }
}

impl fmt::Display for RangeSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.end {
            Some(end) => write!(f, "bytes={}-{}", self.start, end),
            None => write!(f, "bytes={}-", self.start),
        }
    }
}

#[derive(Clone, Debug, Builder)]
#[non_exhaustive]
pub struct RetryPolicy {
    #[builder(default = Duration::from_millis(100))]
    pub base_delay: Duration,
    #[builder(default = Duration::from_secs(5))]
    pub max_delay: Duration,
    #[builder(default = 3)]
    pub max_retries: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl RetryPolicy {
    #[must_use]
    pub fn new(max_retries: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            base_delay,
            max_delay,
            max_retries,
        }
    }

    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        const BACKOFF_BASE: u32 = 2;

        if attempt == 0 {
            return Duration::ZERO;
        }
        let exponential_delay = self.base_delay * BACKOFF_BASE.pow(attempt.saturating_sub(1));
        min(exponential_delay, self.max_delay)
    }
}

#[derive(Clone, Debug, Builder)]
#[non_exhaustive]
pub struct NetOptions {
    /// `Accept-Encoding` algorithms the client offers and auto-decodes.
    /// Defaults to all four (`gzip | deflate | brotli | zstd`); narrow it
    /// when an upstream rejects the full set (anti-bot WAFs that
    /// fingerprint on the exact `Accept-Encoding` string are a common
    /// reason).
    #[builder(default = Compression::all())]
    pub compression: Compression,
    /// Maximum allowed inactivity between consecutive read operations.
    /// Maps to [`reqwest::ClientBuilder::read_timeout`] (documented as
    /// "The timeout applies to each read operation, and resets after a
    /// successful read") and also drives the Downloader-layer
    /// `BodyStream` chunk-inactivity guard for the same semantics one
    /// layer down.
    ///
    /// Protects against zombie connections that send headers but then
    /// stop streaming bytes. Does **not** cap the total request
    /// lifetime; a legitimately slow stream that keeps delivering
    /// chunks (even one byte every few seconds) is not aborted.
    /// Default 30s is sized to absorb realistic mobile-network stalls
    /// (TCP retransmits, captive-portal warm-up, server-side TTFB
    /// spikes) without aborting valid slow streams — the player's
    /// contract is "wait for the segment, regardless of connection
    /// speed", and a 10s cap raced real fixtures.
    #[builder(default = Duration::from_secs(30))]
    pub inactivity_timeout: Duration,
    /// Browser TLS+HTTP2 fingerprint the native `client-wreq` backend
    /// impersonates. Defaults to `Safari`. Ignored by the `client-reqwest`
    /// backend and on wasm32 (no emulation there).
    #[builder(default)]
    pub impersonate: ImpersonatePreset,
    /// Shared byte buffer pool used by backends that must copy platform-owned
    /// response buffers before handing bytes to Rust consumers.
    pub byte_pool: Option<BytePool>,
    #[builder(default)]
    pub retry_policy: RetryPolicy,
    /// Accept invalid TLS certificates (self-signed, expired, wrong hostname).
    /// **Security risk** — use only for local development and test servers.
    #[builder(default)]
    pub is_insecure: bool,
    /// Apple `NSURLSession` streaming body queue capacity, measured in
    /// delivered data chunks waiting for Rust consumption. Set to 0 to disable
    /// `URLSession` task suspension for queued body chunks.
    #[builder(default = 32)]
    pub body_queue_capacity: usize,
    /// Queue length at or below which a suspended Apple streaming task resumes.
    /// Values greater than or equal to [`Self::body_queue_capacity`] are valid:
    /// they resume as soon as the consumer drains one chunk.
    #[builder(default = 16)]
    pub body_queue_resume_at: usize,
    /// Max idle connections per host. Enables HTTP keep-alive connection
    /// reuse, reducing `TIME_WAIT` accumulation under high request volume.
    /// Set to 0 to disable pooling.
    #[builder(default = 8)]
    pub pool_max_idle_per_host: usize,
}

impl Default for NetOptions {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl FromWithParams<Self, Option<BytePool>> for NetOptions {
    fn build(options: Self, byte_pool: Option<BytePool>) -> Self {
        Self::builder()
            .compression(options.compression)
            .inactivity_timeout(options.inactivity_timeout)
            .retry_policy(options.retry_policy)
            .is_insecure(options.is_insecure)
            .pool_max_idle_per_host(options.pool_max_idle_per_host)
            .body_queue_capacity(options.body_queue_capacity)
            .body_queue_resume_at(options.body_queue_resume_at)
            .maybe_byte_pool(options.byte_pool.or(byte_pool))
            .impersonate(options.impersonate)
            .build()
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case::empty_headers(Headers::new(), true)]
    #[case::headers_with_values({
        let mut h = Headers::new();
        h.insert("key1", "value1");
        h.insert("key2", "value2");
        h
    }, false)]
    async fn test_headers_is_empty(#[case] headers: Headers, #[case] expected_empty: bool) {
        assert_eq!(headers.is_empty(), expected_empty);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case::simple_key("key1", "value1")]
    #[case::content_type("Content-Type", "application/json")]
    #[case::custom_header("X-Custom-Header", "custom-value")]
    async fn test_headers_insert_and_get(#[case] key: &str, #[case] value: &str) {
        let mut headers = Headers::new();
        headers.insert(key, value);

        assert_eq!(headers.get(key), Some(value));
        assert_eq!(headers.get("non-existent"), None);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_headers_iter() {
        let mut headers = Headers::new();
        headers.insert("key1", "value1");
        headers.insert("key2", "value2");
        headers.insert("key3", "value3");

        let mut iterated = HashMap::new();
        for (k, v) in headers.iter() {
            iterated.insert(k.to_string(), v.to_string());
        }

        assert_eq!(iterated.len(), 3);
        assert_eq!(iterated.get("key1"), Some(&"value1".to_string()));
        assert_eq!(iterated.get("key2"), Some(&"value2".to_string()));
        assert_eq!(iterated.get("key3"), Some(&"value3".to_string()));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_headers_from_hashmap() {
        let mut map = HashMap::new();
        map.insert("key1".to_string(), "value1".to_string());
        map.insert("key2".to_string(), "value2".to_string());

        let headers: Headers = map.into();

        assert!(!headers.is_empty());
        assert_eq!(headers.get("key1"), Some("value1"));
        assert_eq!(headers.get("key2"), Some("value2"));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_headers_default() {
        let headers = Headers::default();
        assert!(headers.is_empty());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case::full_range(0, Some(100), "bytes=0-100")]
    #[case::open_ended(50, None, "bytes=50-")]
    #[case::single_byte(10, Some(10), "bytes=10-10")]
    #[case::zero_length(0, Some(0), "bytes=0-0")]
    async fn test_range_spec_to_header_value(
        #[case] start: u64,
        #[case] end: Option<u64>,
        #[case] expected_header: &str,
    ) {
        let range = RangeSpec::new(start, end);
        assert_eq!(range.to_string(), expected_header);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case::equal_ranges(RangeSpec::new(0, Some(100)), RangeSpec::new(0, Some(100)), true)]
    #[case::different_starts(RangeSpec::new(0, Some(100)), RangeSpec::new(1, Some(100)), false)]
    #[case::different_ends(RangeSpec::new(0, Some(100)), RangeSpec::new(0, Some(99)), false)]
    #[case::one_open_ended(RangeSpec::new(0, None), RangeSpec::new(0, None), true)]
    #[case::mixed_ends(RangeSpec::new(0, Some(100)), RangeSpec::new(0, None), false)]
    async fn test_range_spec_partial_eq(
        #[case] range1: RangeSpec,
        #[case] range2: RangeSpec,
        #[case] expected_equal: bool,
    ) {
        assert_eq!(range1 == range2, expected_equal);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_range_spec_debug() {
        let range = RangeSpec::new(10, Some(20));
        let debug_output = format!("{:?}", range);
        assert!(debug_output.contains("RangeSpec"));
        assert!(debug_output.contains("start: 10"));
        assert!(debug_output.contains("end: Some(20)"));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_range_spec_clone() {
        let range1 = RangeSpec::new(10, Some(20));
        let range2 = range1.clone();

        assert_eq!(range1, range2);
        assert_eq!(range1.start, range2.start);
        assert_eq!(range1.end, range2.end);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_retry_policy_default() {
        let policy = RetryPolicy::default();

        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.base_delay, Duration::from_millis(100));
        assert_eq!(policy.max_delay, Duration::from_secs(5));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case(1, Duration::from_millis(50), Duration::from_secs(1))]
    #[case(5, Duration::from_millis(100), Duration::from_secs(2))]
    #[case(10, Duration::from_millis(200), Duration::from_secs(10))]
    async fn test_retry_policy_new(
        #[case] max_retries: u32,
        #[case] base_delay: Duration,
        #[case] max_delay: Duration,
    ) {
        let policy = RetryPolicy::new(max_retries, base_delay, max_delay);

        assert_eq!(policy.max_retries, max_retries);
        assert_eq!(policy.base_delay, base_delay);
        assert_eq!(policy.max_delay, max_delay);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case(0, Duration::ZERO)]
    #[case(1, Duration::from_millis(100))]
    #[case(2, Duration::from_millis(200))]
    #[case(3, Duration::from_millis(400))]
    #[case(4, Duration::from_millis(800))]
    #[case(5, Duration::from_millis(1600))]
    #[case(10, Duration::from_secs(5))]
    #[case(20, Duration::from_secs(5))]
    async fn test_retry_policy_delay_for_attempt_default(
        #[case] attempt: u32,
        #[case] expected_delay: Duration,
    ) {
        let policy = RetryPolicy::default();
        let delay = policy.delay_for_attempt(attempt);

        assert_eq!(delay, expected_delay);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case(
        1,
        Duration::from_millis(50),
        Duration::from_millis(200),
        0,
        Duration::ZERO
    )]
    #[case(
        1,
        Duration::from_millis(50),
        Duration::from_millis(200),
        1,
        Duration::from_millis(50)
    )]
    #[case(
        1,
        Duration::from_millis(50),
        Duration::from_millis(200),
        2,
        Duration::from_millis(100)
    )]
    #[case(
        1,
        Duration::from_millis(50),
        Duration::from_millis(200),
        3,
        Duration::from_millis(200)
    )]
    #[case(
        1,
        Duration::from_millis(50),
        Duration::from_millis(200),
        4,
        Duration::from_millis(200)
    )]
    async fn test_retry_policy_delay_for_attempt_custom(
        #[case] max_retries: u32,
        #[case] base_delay: Duration,
        #[case] max_delay: Duration,
        #[case] attempt: u32,
        #[case] expected_delay: Duration,
    ) {
        let policy = RetryPolicy::new(max_retries, base_delay, max_delay);
        let delay = policy.delay_for_attempt(attempt);

        assert_eq!(delay, expected_delay);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_retry_policy_debug() {
        let policy = RetryPolicy::default();
        let debug_output = format!("{:?}", policy);

        assert!(debug_output.contains("RetryPolicy"));
        assert!(debug_output.contains("max_retries: 3"));
        assert!(debug_output.contains("base_delay"));
        assert!(debug_output.contains("max_delay"));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_retry_policy_clone() {
        let policy1 = RetryPolicy::new(5, Duration::from_millis(100), Duration::from_secs(2));
        let policy2 = policy1.clone();

        assert_eq!(policy1.max_retries, policy2.max_retries);
        assert_eq!(policy1.base_delay, policy2.base_delay);
        assert_eq!(policy1.max_delay, policy2.max_delay);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case::start_equals_end(10, Some(10), "bytes=10-10")]
    #[case::start_greater_than_end(20, Some(10), "bytes=20-10")]
    #[case::max_values(u64::MAX, Some(u64::MAX), &format!("bytes={}-{}", u64::MAX, u64::MAX))]
    async fn test_range_spec_edge_cases(
        #[case] start: u64,
        #[case] end: Option<u64>,
        #[case] expected_header: &str,
    ) {
        let range = RangeSpec::new(start, end);
        assert_eq!(range.to_string(), expected_header);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case::zero_max_retries(0, Duration::from_millis(100), Duration::from_secs(1))]
    #[case::large_max_retries(100, Duration::from_millis(10), Duration::from_secs(10))]
    #[case::zero_base_delay(3, Duration::ZERO, Duration::from_secs(1))]
    #[case::zero_max_delay(3, Duration::from_millis(100), Duration::ZERO)]
    async fn test_retry_policy_edge_cases(
        #[case] max_retries: u32,
        #[case] base_delay: Duration,
        #[case] max_delay: Duration,
    ) {
        let policy = RetryPolicy::new(max_retries, base_delay, max_delay);

        for attempt in 0..=5 {
            let delay = policy.delay_for_attempt(attempt);

            assert!(delay >= Duration::ZERO);

            assert!(delay <= max_delay);

            if base_delay == Duration::ZERO {
                assert_eq!(delay, Duration::ZERO);
            }
        }
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case(10)]
    #[case(20)]
    async fn test_retry_policy_large_attempts(#[case] attempt: u32) {
        let policy = RetryPolicy::default();

        let delay = policy.delay_for_attempt(attempt);

        assert!(delay <= policy.max_delay);
        assert!(delay >= Duration::ZERO);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    #[case::with_spaces("X-Custom Header", "value with spaces")]
    #[case::with_unicode("X-Emoji", "🎉")]
    #[case::with_special_chars("X-Special", "a\tb\nc")]
    #[case::empty_value("X-Empty", "")]
    async fn test_headers_special_characters(#[case] key: &str, #[case] value: &str) {
        let mut headers = Headers::new();
        headers.insert(key, value);

        assert_eq!(headers.get(key), Some(value));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn test_headers_case_sensitive() {
        let mut headers = Headers::new();
        headers.insert("Content-Type", "application/json");
        headers.insert("content-type", "text/plain");

        assert_eq!(headers.get("Content-Type"), Some("application/json"));
        assert_eq!(headers.get("content-type"), Some("text/plain"));
        assert_ne!(headers.get("Content-Type"), headers.get("content-type"));
    }
}
