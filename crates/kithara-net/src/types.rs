use std::{cmp::min, collections::HashMap, time::Duration};

#[derive(Clone, Debug, PartialEq)]
pub struct Headers {
    inner: HashMap<String, String>,
}

impl Headers {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn insert<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
        self.inner.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl Default for Headers {
    fn default() -> Self {
        Self::new()
    }
}

impl From<HashMap<String, String>> for Headers {
    fn from(map: HashMap<String, String>) -> Self {
        Self { inner: map }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RangeSpec {
    pub start: u64,
    pub end: Option<u64>,
}

impl RangeSpec {
    pub fn new(start: u64, end: Option<u64>) -> Self {
        Self { start, end }
    }

    pub fn from_start(start: u64) -> Self {
        Self { start, end: None }
    }

    pub fn to_header_value(&self) -> String {
        if let Some(end) = self.end {
            format!("bytes={}-{}", self.start, end)
        } else {
            format!("bytes={}-", self.start)
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
        }
    }
}

impl RetryPolicy {
    pub fn new(max_retries: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_retries,
            base_delay,
            max_delay,
        }
    }

    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }

        let exponential_delay = self.base_delay * 2_u32.pow(attempt.saturating_sub(1));
        min(exponential_delay, self.max_delay)
    }
}

#[derive(Clone, Debug)]
pub struct NetOptions {
    pub request_timeout: Duration,
    pub retry_policy: RetryPolicy,
    /// Max idle connections per host. Set to 0 to disable pooling and reduce memory.
    pub pool_max_idle_per_host: usize,
}

impl Default for NetOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            retry_policy: RetryPolicy::default(),
            pool_max_idle_per_host: 0, // Disable pooling for lower memory
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::*;

    use super::*;

    // Headers tests
    #[rstest]
    #[case::empty_headers(Headers::new(), true)]
    #[case::headers_with_values({
        let mut h = Headers::new();
        h.insert("key1", "value1");
        h.insert("key2", "value2");
        h
    }, false)]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_headers_is_empty(#[case] headers: Headers, #[case] expected_empty: bool) {
        assert_eq!(headers.is_empty(), expected_empty);
    }

    #[rstest]
    #[case::insert_and_get("key1", "value1")]
    #[case::insert_and_get("Content-Type", "application/json")]
    #[case::insert_and_get("X-Custom-Header", "custom-value")]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_headers_insert_and_get(#[case] key: &str, #[case] value: &str) {
        let mut headers = Headers::new();
        headers.insert(key, value);

        assert_eq!(headers.get(key), Some(value));
        assert_eq!(headers.get("non-existent"), None);
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
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

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_headers_from_hashmap() {
        let mut map = HashMap::new();
        map.insert("key1".to_string(), "value1".to_string());
        map.insert("key2".to_string(), "value2".to_string());

        let headers: Headers = map.into();

        assert!(!headers.is_empty());
        assert_eq!(headers.get("key1"), Some("value1"));
        assert_eq!(headers.get("key2"), Some("value2"));
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_headers_default() {
        let headers = Headers::default();
        assert!(headers.is_empty());
    }

    // RangeSpec tests
    #[rstest]
    #[case::full_range(0, Some(100), "bytes=0-100")]
    #[case::open_ended(50, None, "bytes=50-")]
    #[case::single_byte(10, Some(10), "bytes=10-10")]
    #[case::zero_length(0, Some(0), "bytes=0-0")]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_range_spec_to_header_value(
        #[case] start: u64,
        #[case] end: Option<u64>,
        #[case] expected_header: &str,
    ) {
        let range = RangeSpec::new(start, end);
        assert_eq!(range.to_header_value(), expected_header);
    }

    #[rstest]
    #[case::from_start_0(0, 0, None)]
    #[case::from_start_100(100, 100, None)]
    #[case::from_start_max(u64::MAX, u64::MAX, None)]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_range_spec_from_start(
        #[case] start: u64,
        #[case] expected_start: u64,
        #[case] expected_end: Option<u64>,
    ) {
        let range = RangeSpec::from_start(start);
        assert_eq!(range.start, expected_start);
        assert_eq!(range.end, expected_end);
    }

    #[rstest]
    #[case::equal_ranges(RangeSpec::new(0, Some(100)), RangeSpec::new(0, Some(100)), true)]
    #[case::different_starts(RangeSpec::new(0, Some(100)), RangeSpec::new(1, Some(100)), false)]
    #[case::different_ends(RangeSpec::new(0, Some(100)), RangeSpec::new(0, Some(99)), false)]
    #[case::one_open_ended(RangeSpec::new(0, None), RangeSpec::new(0, None), true)]
    #[case::mixed_ends(RangeSpec::new(0, Some(100)), RangeSpec::new(0, None), false)]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_range_spec_partial_eq(
        #[case] range1: RangeSpec,
        #[case] range2: RangeSpec,
        #[case] expected_equal: bool,
    ) {
        assert_eq!(range1 == range2, expected_equal);
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_range_spec_debug() {
        let range = RangeSpec::new(10, Some(20));
        let debug_output = format!("{:?}", range);
        assert!(debug_output.contains("RangeSpec"));
        assert!(debug_output.contains("start: 10"));
        assert!(debug_output.contains("end: Some(20)"));
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_range_spec_clone() {
        let range1 = RangeSpec::new(10, Some(20));
        let range2 = range1.clone();

        assert_eq!(range1, range2);
        assert_eq!(range1.start, range2.start);
        assert_eq!(range1.end, range2.end);
    }

    // RetryPolicy tests
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_retry_policy_default() {
        let policy = RetryPolicy::default();

        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.base_delay, Duration::from_millis(100));
        assert_eq!(policy.max_delay, Duration::from_secs(5));
    }

    #[rstest]
    #[case(1, Duration::from_millis(50), Duration::from_secs(1))]
    #[case(5, Duration::from_millis(100), Duration::from_secs(2))]
    #[case(10, Duration::from_millis(200), Duration::from_secs(10))]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
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

    #[rstest]
    #[case(0, Duration::ZERO)]
    #[case(1, Duration::from_millis(100))]
    #[case(2, Duration::from_millis(200))]
    #[case(3, Duration::from_millis(400))]
    #[case(4, Duration::from_millis(800))]
    #[case(5, Duration::from_millis(1600))]
    #[case(10, Duration::from_secs(5))] // Capped at max_delay
    #[case(20, Duration::from_secs(5))] // Capped at max_delay
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_retry_policy_delay_for_attempt_default(
        #[case] attempt: u32,
        #[case] expected_delay: Duration,
    ) {
        let policy = RetryPolicy::default();
        let delay = policy.delay_for_attempt(attempt);

        assert_eq!(delay, expected_delay);
    }

    #[rstest]
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
    )] // Capped
    #[case(
        1,
        Duration::from_millis(50),
        Duration::from_millis(200),
        4,
        Duration::from_millis(200)
    )] // Capped
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
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

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_retry_policy_debug() {
        let policy = RetryPolicy::default();
        let debug_output = format!("{:?}", policy);

        assert!(debug_output.contains("RetryPolicy"));
        assert!(debug_output.contains("max_retries: 3"));
        assert!(debug_output.contains("base_delay"));
        assert!(debug_output.contains("max_delay"));
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_retry_policy_clone() {
        let policy1 = RetryPolicy::new(5, Duration::from_millis(100), Duration::from_secs(2));
        let policy2 = policy1.clone();

        assert_eq!(policy1.max_retries, policy2.max_retries);
        assert_eq!(policy1.base_delay, policy2.base_delay);
        assert_eq!(policy1.max_delay, policy2.max_delay);
    }

    // Edge cases for RangeSpec
    #[rstest]
    #[case::start_equals_end(10, Some(10), "bytes=10-10")]
    #[case::start_greater_than_end(20, Some(10), "bytes=20-10")] // This is valid per spec
    #[case::max_values(u64::MAX, Some(u64::MAX), &format!("bytes={}-{}", u64::MAX, u64::MAX))]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_range_spec_edge_cases(
        #[case] start: u64,
        #[case] end: Option<u64>,
        #[case] expected_header: &str,
    ) {
        let range = RangeSpec::new(start, end);
        assert_eq!(range.to_header_value(), expected_header);
    }

    // Edge cases for RetryPolicy
    #[rstest]
    #[case::zero_max_retries(0, Duration::from_millis(100), Duration::from_secs(1))]
    #[case::large_max_retries(100, Duration::from_millis(10), Duration::from_secs(10))]
    #[case::zero_base_delay(3, Duration::ZERO, Duration::from_secs(1))]
    #[case::zero_max_delay(3, Duration::from_millis(100), Duration::ZERO)]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_retry_policy_edge_cases(
        #[case] max_retries: u32,
        #[case] base_delay: Duration,
        #[case] max_delay: Duration,
    ) {
        let policy = RetryPolicy::new(max_retries, base_delay, max_delay);

        // Test delay calculation for edge cases
        for attempt in 0..=5 {
            let delay = policy.delay_for_attempt(attempt);

            // Delay should never be negative
            assert!(delay >= Duration::ZERO);

            // Delay should be capped at max_delay
            assert!(delay <= max_delay);

            // For zero base_delay, all delays should be zero (except attempt 0 which is always zero)
            if base_delay == Duration::ZERO {
                assert_eq!(delay, Duration::ZERO);
            }
        }
    }

    // Test that large attempt numbers don't cause overflow
    #[rstest]
    #[case(10)]
    #[case(20)]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_retry_policy_large_attempts(#[case] attempt: u32) {
        let policy = RetryPolicy::default();

        // This should not panic
        let delay = policy.delay_for_attempt(attempt);

        // Delay should be capped at max_delay
        assert!(delay <= policy.max_delay);
        assert!(delay >= Duration::ZERO);
    }

    // Test Headers with special characters
    #[rstest]
    #[case::with_spaces("X-Custom Header", "value with spaces")]
    #[case::with_unicode("X-Emoji", "🎉")]
    #[case::with_special_chars("X-Special", "a\tb\nc")]
    #[case::empty_value("X-Empty", "")]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_headers_special_characters(#[case] key: &str, #[case] value: &str) {
        let mut headers = Headers::new();
        headers.insert(key, value);

        assert_eq!(headers.get(key), Some(value));
    }

    // Test Headers case sensitivity
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_headers_case_sensitive() {
        let mut headers = Headers::new();
        headers.insert("Content-Type", "application/json");
        headers.insert("content-type", "text/plain");

        // Should be case-sensitive
        assert_eq!(headers.get("Content-Type"), Some("application/json"));
        assert_eq!(headers.get("content-type"), Some("text/plain"));
        assert_ne!(headers.get("Content-Type"), headers.get("content-type"));
    }
}
