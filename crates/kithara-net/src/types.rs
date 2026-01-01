
use std::collections::HashMap;
use std::time::Duration;

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

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.inner.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(|s| s.as_str())
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
pub struct NetOptions {
    pub request_timeout: Duration,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub max_retry_delay: Duration,
}

impl Default for NetOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
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
        std::cmp::min(exponential_delay, self.max_delay)
    }
}