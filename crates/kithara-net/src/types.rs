use std::{cmp::min, collections::HashMap, fmt};

use bitflags::bitflags;
use kithara_config::Config;
use kithara_derive::Patch;
use kithara_platform::time::Duration;
use serde::{Deserialize, Deserializer};

use crate::error::{NetError, Retryability};

bitflags! {
    /// HTTP codings auto-decoded for whole-body native requests.
    /// Byte-addressed requests use `identity`; configured subsets stay exact.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Compression: u8 {
        const GZIP    = 1 << 0;
        const DEFLATE = 1 << 1;
        const BROTLI  = 1 << 2;
        const ZSTD    = 1 << 3;
    }
}

/// One `Accept-Encoding` algorithm. Named rather than spelled as bit flags,
/// because a document reads as a list of names.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompressionAlgorithm {
    Gzip,
    Deflate,
    #[serde(alias = "br")]
    Brotli,
    Zstd,
}

impl From<CompressionAlgorithm> for Compression {
    fn from(algorithm: CompressionAlgorithm) -> Self {
        match algorithm {
            CompressionAlgorithm::Gzip => Self::GZIP,
            CompressionAlgorithm::Deflate => Self::DEFLATE,
            CompressionAlgorithm::Brotli => Self::BROTLI,
            CompressionAlgorithm::Zstd => Self::ZSTD,
        }
    }
}

/// Read as the list of algorithm names a document spells, so the flags stay an
/// implementation detail of this crate. An empty list is [`Compression::empty`]
/// -- negotiation off.
impl<'de> Deserialize<'de> for Compression {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<CompressionAlgorithm>::deserialize(deserializer).map(|algorithms| {
            algorithms
                .into_iter()
                .fold(Self::empty(), |flags, algorithm| {
                    flags.union(algorithm.into())
                })
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcceptEncodingPolicy {
    Configured,
    Identity,
}

#[cfg(not(any(target_arch = "wasm32", feature = "client-host")))]
pub(crate) fn accept_encoding_value(compression: Compression) -> String {
    let mut codings: Vec<&'static str> = Vec::new();
    if compression.contains(Compression::GZIP) {
        codings.push("gzip");
    }
    if compression.contains(Compression::DEFLATE) {
        codings.push("deflate");
    }
    if compression.contains(Compression::BROTLI) {
        codings.push("br");
    }
    if compression.contains(Compression::ZSTD) {
        codings.push("zstd");
    }
    if codings.is_empty() {
        return "identity".to_string();
    }
    codings.join(", ")
}

/// TLS+HTTP fingerprint the native `client-wreq` backend impersonates. The
/// DRM keyserver sits behind an anti-bot WAF that fingerprints the TLS
/// `ClientHello` (JA3) and 418-rejects non-browser stacks; presenting a real
/// browser fingerprint via `wreq` is what gets a 200. Default `Safari` matches
/// iOS `URLSession`. Inert under the `client-reqwest` backend and on wasm32
/// (the browser fetch already carries a real fingerprint).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImpersonatePreset {
    #[default]
    Safari,
    Chrome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    inner: HashMap<String, (String, String)>,
}

impl Headers {
    pub fn insert<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
        let name = key.into();
        self.inner
            .insert(name.to_ascii_lowercase(), (name, value.into()));
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner
            .get(&key.to_ascii_lowercase())
            .map(|(_, value)| value.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner
            .values()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    delegate::delegate! {
        to self.inner {
            #[must_use]
            pub fn is_empty(&self) -> bool;
        }
    }
}

impl From<HashMap<String, String>> for Headers {
    fn from(map: HashMap<String, String>) -> Self {
        let mut headers = Self::default();
        for (name, value) in map {
            headers.insert(name, value);
        }
        headers
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeSpec {
    pub end: Option<u64>,
    pub start: u64,
}

impl RangeSpec {
    #[must_use]
    pub const fn new(start: u64, end: Option<u64>) -> Self {
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

#[derive(Clone, Copy, Debug, Config, Eq, PartialEq, Patch)]
#[config(default, fields(value))]
#[non_exhaustive]
pub struct RetryPolicy {
    #[config(builder(default = Duration::from_millis(100)), patch(humantime))]
    pub(crate) base_delay: Duration,
    #[config(builder(default = Duration::from_secs(5)), patch(humantime))]
    pub(crate) max_delay: Duration,
    #[config(builder(default = 3))]
    pub(crate) max_retries: u32,
}

impl RetryPolicy {
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        const BACKOFF_BASE: u32 = 2;

        if attempt == 0 {
            return Duration::ZERO;
        }
        let exponential_delay = self.base_delay * BACKOFF_BASE.pow(attempt.saturating_sub(1));
        min(exponential_delay, self.max_delay)
    }

    /// Whether `attempt` may be retried: budget left and the error is transient.
    #[must_use]
    pub fn should_retry(&self, error: &NetError, attempt: u32) -> bool {
        if attempt >= self.max_retries {
            return false;
        }

        error.retryability() == Retryability::Transient
    }
}

#[derive(Clone, Debug, Config, Patch)]
#[config(default, fields(value))]
#[non_exhaustive]
pub struct NetOptions {
    /// Codings advertised and decoded for whole-body native requests.
    /// Defaults to all four; byte-addressed requests always use `identity`.
    #[config(builder(default = Compression::all()), patch(attribute(serde(default))))]
    pub(crate) compression: Compression,
    /// Maximum wait for response headers or inactivity between body chunks.
    /// Progress resets the runtime streaming layer's timer; it never caps a live transfer's total duration.
    /// Defaults to 30 seconds to tolerate mobile-network stalls.
    #[config(builder(default = Duration::from_secs(30)), patch(humantime))]
    pub(crate) inactivity_timeout: Duration,
    /// How long a pooled connection may sit idle before it is dropped.
    /// Governs the same pool as [`Self::pool_max_idle_per_host`]: the count
    /// caps how many idle connections survive, this caps how long. The
    /// default matches the keep-alive window CDNs commonly advertise, so a
    /// segment burst reuses connections while a paused player does not hold
    /// sockets open. Ignored by the Apple backend, whose `URLSession`
    /// configuration exposes no idle-pool timeout.
    #[config(builder(default = Duration::from_secs(5)), patch(humantime))]
    pub(crate) pool_idle_timeout: Duration,
    /// Browser TLS+HTTP2 fingerprint the native `client-wreq` backend
    /// impersonates. Defaults to `Safari`. Ignored by the `client-reqwest`
    /// backend and on wasm32 (no emulation there).
    #[config(builder(default))]
    pub(crate) impersonate: ImpersonatePreset,
    #[config(nested, builder(default), patch(nested))]
    pub(crate) retry_policy: RetryPolicy,
    /// Accept invalid TLS certificates (self-signed, expired, wrong hostname).
    /// **Security risk** — use only for local development and test servers.
    #[config(builder(default))]
    pub(crate) is_insecure: bool,
    /// Apple `NSURLSession` streaming body queue capacity, measured in
    /// delivered data chunks waiting for Rust consumption. Set to 0 to disable
    /// `URLSession` task suspension for queued body chunks.
    #[config(builder(default = 32))]
    pub(crate) body_queue_capacity: usize,
    /// Queue length at or below which a suspended Apple streaming task resumes.
    /// Values greater than or equal to [`Self::body_queue_capacity`] are valid:
    /// they resume as soon as the consumer drains one chunk.
    #[config(builder(default = 16))]
    pub(crate) body_queue_resume_at: usize,
    /// Max idle connections per host. Enables HTTP keep-alive connection
    /// reuse, reducing `TIME_WAIT` accumulation under high request volume.
    /// Set to 0 to disable pooling.
    #[config(builder(default = 8))]
    pub(crate) pool_max_idle_per_host: usize,
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test(tokio)]
    #[case::empty_headers(Headers::default(), true)]
    #[case::headers_with_values({
        let mut h = Headers::default();
        h.insert("key1", "value1");
        h.insert("key2", "value2");
        h
    }, false)]
    async fn test_headers_is_empty(#[case] headers: Headers, #[case] expected_empty: bool) {
        assert_eq!(headers.is_empty(), expected_empty);
    }

    #[kithara::test(tokio)]
    #[case::simple_key("key1", "value1")]
    #[case::content_type("Content-Type", "application/json")]
    #[case::custom_header("X-Custom-Header", "custom-value")]
    #[case::with_spaces("X-Custom Header", "value with spaces")]
    #[case::with_unicode("X-Emoji", "\u{1f389}")]
    #[case::with_special_chars("X-Special", "a\tb\nc")]
    #[case::empty_value("X-Empty", "")]
    async fn test_headers_insert_and_get(#[case] key: &str, #[case] value: &str) {
        let mut headers = Headers::default();
        headers.insert(key, value);

        assert_eq!(headers.get(key), Some(value));
        assert_eq!(headers.get("non-existent"), None);
    }

    #[kithara::test(tokio)]
    async fn test_headers_iter() {
        let mut headers = Headers::default();
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

    #[kithara::test(tokio)]
    #[case::full_range(0, Some(100), "bytes=0-100")]
    #[case::open_ended(50, None, "bytes=50-")]
    #[case::single_byte(10, Some(10), "bytes=10-10")]
    #[case::zero_length(0, Some(0), "bytes=0-0")]
    #[case::start_greater_than_end(20, Some(10), "bytes=20-10")]
    #[case::max_values(u64::MAX, Some(u64::MAX), &format!("bytes={}-{}", u64::MAX, u64::MAX))]
    async fn test_range_spec_to_header_value(
        #[case] start: u64,
        #[case] end: Option<u64>,
        #[case] expected_header: &str,
    ) {
        let range = RangeSpec::new(start, end);
        assert_eq!(range.to_string(), expected_header);
    }

    #[kithara::test(tokio)]
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

    #[kithara::test(tokio)]
    async fn test_range_spec_debug() {
        let range = RangeSpec::new(10, Some(20));
        let debug_output = format!("{:?}", range);
        assert!(debug_output.contains("RangeSpec"));
        assert!(debug_output.contains("start: 10"));
        assert!(debug_output.contains("end: Some(20)"));
    }

    #[kithara::test(tokio)]
    async fn test_range_spec_clone() {
        let range1 = RangeSpec::new(10, Some(20));
        let range2 = range1.clone();

        assert_eq!(range1, range2);
        assert_eq!(range1.start, range2.start);
        assert_eq!(range1.end, range2.end);
    }

    #[kithara::test(tokio)]
    async fn test_retry_policy_default() {
        let policy = RetryPolicy::default();

        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.base_delay, Duration::from_millis(100));
        assert_eq!(policy.max_delay, Duration::from_secs(5));
    }

    #[kithara::test(tokio)]
    #[case(1, Duration::from_millis(50), Duration::from_secs(1))]
    #[case(5, Duration::from_millis(100), Duration::from_secs(2))]
    #[case(10, Duration::from_millis(200), Duration::from_secs(10))]
    async fn test_retry_policy_builder(
        #[case] max_retries: u32,
        #[case] base_delay: Duration,
        #[case] max_delay: Duration,
    ) {
        let policy = RetryPolicy::builder()
            .max_retries(max_retries)
            .base_delay(base_delay)
            .max_delay(max_delay)
            .build();

        assert_eq!(policy.max_retries, max_retries);
        assert_eq!(policy.base_delay, base_delay);
        assert_eq!(policy.max_delay, max_delay);
    }

    #[kithara::test(tokio)]
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

    #[kithara::test(tokio)]
    #[case(0, Duration::ZERO)]
    #[case(1, Duration::from_millis(50))]
    #[case(2, Duration::from_millis(100))]
    #[case(3, Duration::from_millis(200))]
    #[case(4, Duration::from_millis(200))]
    async fn test_retry_policy_delay_for_attempt_custom(
        #[case] attempt: u32,
        #[case] expected_delay: Duration,
    ) {
        let policy = RetryPolicy::builder()
            .max_retries(1)
            .base_delay(Duration::from_millis(50))
            .max_delay(Duration::from_millis(200))
            .build();
        let delay = policy.delay_for_attempt(attempt);

        assert_eq!(delay, expected_delay);
    }

    #[kithara::test(tokio)]
    async fn test_retry_policy_debug() {
        let policy = RetryPolicy::default();
        let debug_output = format!("{:?}", policy);

        assert!(debug_output.contains("RetryPolicy"));
        assert!(debug_output.contains("max_retries: 3"));
        assert!(debug_output.contains("base_delay"));
        assert!(debug_output.contains("max_delay"));
    }

    #[kithara::test(tokio)]
    #[case::zero_max_retries(0, Duration::from_millis(100), Duration::from_secs(1))]
    #[case::large_max_retries(100, Duration::from_millis(10), Duration::from_secs(10))]
    #[case::zero_base_delay(3, Duration::ZERO, Duration::from_secs(1))]
    #[case::zero_max_delay(3, Duration::from_millis(100), Duration::ZERO)]
    async fn test_retry_policy_edge_cases(
        #[case] max_retries: u32,
        #[case] base_delay: Duration,
        #[case] max_delay: Duration,
    ) {
        let policy = RetryPolicy::builder()
            .max_retries(max_retries)
            .base_delay(base_delay)
            .max_delay(max_delay)
            .build();

        for attempt in 0..=5 {
            let delay = policy.delay_for_attempt(attempt);

            assert!(delay >= Duration::ZERO);

            assert!(delay <= max_delay);

            if base_delay == Duration::ZERO {
                assert_eq!(delay, Duration::ZERO);
            }
        }
    }

    #[kithara::test(tokio)]
    #[case(10)]
    #[case(20)]
    async fn test_retry_policy_large_attempts(#[case] attempt: u32) {
        let policy = RetryPolicy::default();

        let delay = policy.delay_for_attempt(attempt);

        assert!(delay <= policy.max_delay);
        assert!(delay >= Duration::ZERO);
    }

    #[kithara::test(tokio)]
    #[case::insert_then_lookup({
        let mut h = Headers::default();
        h.insert("content-length", "13");
        h
    }, "Content-Length", "13", &[("content-length", "13")])]
    #[case::two_spellings_one_entry({
        let mut h = Headers::default();
        h.insert("content-type", "a");
        h.insert("Content-Type", "b");
        h
    }, "CONTENT-TYPE", "b", &[("Content-Type", "b")])]
    #[case::from_map(
        Headers::from(HashMap::from([("Content-Length".to_owned(), "5".to_owned())])),
        "content-length",
        "5",
        &[("Content-Length", "5")]
    )]
    async fn a_header_name_matches_in_any_case(
        #[case] headers: Headers,
        #[case] lookup: &str,
        #[case] value: &str,
        #[case] entries: &[(&str, &str)],
    ) {
        assert_eq!(headers.get(lookup), Some(value));
        assert_eq!(headers.iter().collect::<Vec<_>>(), entries);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod settings_tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::{Compression, Duration, NetOptions, NetOptionsPatch, RetryPolicy};

    #[kithara::test(native, flash(false))]
    #[case::default_deadline(30)]
    #[case::custom_deadline(90)]
    #[case::long_deadline(300)]
    #[case::extended_deadline(1_000)]
    fn a_patch_writes_only_the_fields_it_names(#[case] timeout_secs: u64) {
        let document = format!("inactivity_timeout: {timeout_secs}s\npool_idle_timeout: 250ms\n");
        let settings: NetOptionsPatch =
            serde_yaml_ng::from_str(&document).expect("the document types");
        let mut options = NetOptions::builder().body_queue_capacity(7).build();

        options.apply(settings);

        assert_eq!(
            options.inactivity_timeout,
            Duration::from_secs(timeout_secs)
        );
        assert_eq!(options.pool_idle_timeout, Duration::from_millis(250));
        assert_eq!(
            options.body_queue_capacity, 7,
            "a silent field must keep the value it already had"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_nested_retry_patch_reaches_the_inner_field() {
        let settings: NetOptionsPatch =
            serde_yaml_ng::from_str("retry_policy:\n  max_retries: 7\n  base_delay: 250ms\n")
                .expect("the document types");
        let mut options = NetOptions::builder()
            .retry_policy(
                RetryPolicy::builder()
                    .max_delay(Duration::from_secs(11))
                    .build(),
            )
            .build();

        options.apply(settings);

        assert_eq!(options.retry_policy.max_retries, 7);
        assert_eq!(options.retry_policy.base_delay, Duration::from_millis(250));
        assert_eq!(
            options.retry_policy.max_delay,
            Duration::from_secs(11),
            "a silent inner field must keep the value it already had"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_unknown_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<NetOptionsPatch>("inactivity_timeout_ms: 1s\n")
            .expect_err("a typo must not be silently ignored");

        assert!(
            format!("{error}").contains("inactivity_timeout_ms"),
            "{error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn compression_is_read_as_the_list_of_names_a_document_spells() {
        let settings: NetOptionsPatch =
            serde_yaml_ng::from_str("compression: [gzip, deflate]\n").expect("the document types");
        let mut options = NetOptions::builder().build();

        options.apply(settings);

        assert_eq!(
            options.compression,
            Compression::GZIP.union(Compression::DEFLATE)
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_empty_compression_list_disables_negotiation() {
        let settings: NetOptionsPatch =
            serde_yaml_ng::from_str("compression: []\n").expect("the document types");
        let mut options = NetOptions::builder().build();

        options.apply(settings);

        assert_eq!(options.compression, Compression::empty());
    }
}
