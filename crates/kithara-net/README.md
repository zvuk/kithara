# `kithara-net` — networking primitives for Kithara

`kithara-net` provides HTTP networking abstractions for Kithara, with support for:
- Progressive downloads with range requests
- Retry logic with configurable policies
- Timeout handling
- Streaming response bodies
- Header manipulation

This crate is designed to be generic over HTTP clients while providing a consistent interface
for higher-level crates like `kithara-file` and `kithara-hls`.

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core abstractions
- `trait Net` — Base networking trait for HTTP operations
- `trait NetExt` — Extension methods for composition (`with_timeout`, `with_retry`)
- `type ByteStream` — Stream of bytes from network responses

### Concrete implementations
- `struct HttpClient` — `reqwest`-based HTTP client implementation
- `struct TimeoutNet<N>` — Decorator that adds timeout handling to any `Net` implementation
- `struct RetryNet<N, P>` — Decorator that adds retry logic to any `Net` implementation

### Configuration and types
- `struct NetOptions` — Configuration for `HttpClient`
- `struct RetryPolicy` — Retry behavior configuration
- `struct RangeSpec` — Range request specification (start, end)
- `struct Headers` — HTTP headers wrapper

### Error handling
- `enum NetError` — Error type for network operations
- `type NetResult<T> = Result<T, NetError>` — Result alias

## Core invariants

1. **Streaming responses**: Large responses are streamed, not buffered entirely in memory
2. **Range support**: All implementations support HTTP range requests for progressive downloads
3. **Retry semantics**: Retries follow configurable policies with exponential backoff
4. **Timeout handling**: Operations have configurable timeouts to prevent hangs
5. **Error classification**: Errors are categorized (Http, Timeout, InvalidRange) for appropriate handling

## `Net` trait

The `Net` trait defines the core networking operations:

```rust
#[async_trait]
pub trait Net: Send + Sync {
    /// Get all bytes from a URL
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError>;

    /// Stream bytes from a URL
    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError>;

    /// Get a range of bytes from a URL
    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError>;

    /// Perform a HEAD request
    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError>;
}
```

Key aspects:
- All methods take `Url` (from `url` crate) instead of string slices
- Optional headers for authentication, content negotiation, etc.
- Returns `ByteStream` for progressive reading or `Bytes` for complete download
- No explicit cancellation token (use tokio task cancellation)

## `NetExt` trait

Provides decorator methods built on top of the `Net` trait:

```rust
pub trait NetExt: Net + Sized {
    /// Add timeout layer
    fn with_timeout(self, timeout: Duration) -> TimeoutNet<Self>;

    /// Add retry layer
    fn with_retry(self, policy: RetryPolicy) -> RetryNet<Self, DefaultRetryPolicy>;
}
```

Usage example:
```rust
let client = HttpClient::default()
    .with_timeout(Duration::from_secs(30))
    .with_retry(RetryPolicy::default());
```

## `ByteStream` type

Stream of bytes from network responses:

```rust
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>;
```

Designed for:
- Progressive reading of large responses
- Integration with `kithara-stream` orchestration
- Efficient memory usage (stream chunks as they arrive)
- Compatible with `futures::StreamExt` combinators

## Error handling

`NetError` categorizes network failures:

- `Http(String)` — Generic HTTP error with message
- `InvalidRange(String)` — Invalid range request
- `Timeout` — Operation timed out
- `RetryExhausted { max_retries, source }` — All retries failed
- `HttpError { status, url, body }` — HTTP error with details
- `Unimplemented` — Placeholder for unimplemented features

Each variant includes context about the operation for debugging.

Methods:
- `is_retryable() -> bool` — Check if error should be retried (5xx, 429, 408, timeouts)

## Configuration

### `NetOptions`
Configuration for `HttpClient`:

```rust
pub struct NetOptions {
    pub request_timeout: Duration,
    pub retry_policy: RetryPolicy,
    pub pool_max_idle_per_host: usize,
}
```

Defaults:
- `request_timeout`: 30 seconds
- `retry_policy`: `RetryPolicy::default()`
- `pool_max_idle_per_host`: 0 (pooling disabled for lower memory)

### `RetryPolicy`
Retry behavior configuration:

```rust
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}
```

Defaults:
- `max_retries`: 3
- `base_delay`: 100ms
- `max_delay`: 5s

Delay calculation uses exponential backoff: `base_delay * 2^(attempt-1)`, capped at `max_delay`.

### `RangeSpec`
Range request specification:

```rust
pub struct RangeSpec {
    pub start: u64,
    pub end: Option<u64>,
}
```

Methods:
- `new(start, end)` — Create range with optional end
- `from_start(start)` — Open-ended range from start to EOF
- `to_header_value()` — Convert to HTTP Range header value (e.g., `"bytes=0-100"`)

### `Headers`
HTTP headers wrapper:

```rust
pub struct Headers {
    // HashMap<String, String>
}
```

Methods:
- `new()` — Create empty headers
- `insert(key, value)` — Add header
- `get(key)` — Get header value
- `iter()` — Iterate over headers
- `is_empty()` — Check if empty

## Integration with other Kithara crates

### `kithara-file`
- Uses `Net` for progressive MP3/AAC downloads
- Implements `Source` using HTTP client
- Handles range requests for seeking

### `kithara-hls`
- Uses `Net` for playlist and segment downloads
- Implements retry logic for segment failures
- Uses decorators for timeout and retry composition

## Example usage

### Basic HTTP GET
```rust
use kithara_net::HttpClient;
use url::Url;

let client = HttpClient::default();
let url = Url::parse("https://example.com/audio.mp3")?;

// Get all bytes at once
let bytes = client.get_bytes(url.clone(), None).await?;

// Or stream progressively
let mut stream = client.stream(url, None).await?;
use futures::StreamExt;
while let Some(chunk) = stream.next().await {
    let chunk = chunk?;
    // Process chunk
}
```

### Range request
```rust
use kithara_net::{HttpClient, RangeSpec};
use url::Url;

let client = HttpClient::default();
let url = Url::parse("https://example.com/large.mp3")?;
let range = RangeSpec::from_start(1024); // Start from byte 1024

let mut stream = client.get_range(url, range, None).await?;
```

### With timeout and retry
```rust
use kithara_net::{HttpClient, NetExt, RetryPolicy};
use std::time::Duration;
use url::Url;

let client = HttpClient::default()
    .with_timeout(Duration::from_secs(30))
    .with_retry(RetryPolicy::default());

let url = Url::parse("https://example.com/audio.mp3")?;
let bytes = client.get_bytes(url, None).await?;
```

### With custom headers
```rust
use kithara_net::{HttpClient, Headers};
use url::Url;

let mut headers = Headers::new();
headers.insert("Authorization", "Bearer token");
headers.insert("Accept", "audio/mpeg");

let client = HttpClient::default();
let url = Url::parse("https://example.com/audio.mp3")?;
let bytes = client.get_bytes(url, Some(headers)).await?;
```

### HEAD request
```rust
use kithara_net::HttpClient;
use url::Url;

let client = HttpClient::default();
let url = Url::parse("https://example.com/audio.mp3")?;

let headers = client.head(url, None).await?;
if let Some(content_length) = headers.get("Content-Length") {
    println!("File size: {}", content_length);
}
```

## Design philosophy

1. **Abstraction over implementation**: `Net` trait allows switching HTTP clients
2. **Composition over inheritance**: Decorators (`TimeoutNet`, `RetryNet`) add functionality
3. **Async-first**: All operations designed for tokio async runtime
4. **Progressive loading**: Stream-based API for large resources
5. **Configurable policies**: Retry, timeout, and other behaviors are configurable
6. **Type safety**: Use `Url` type instead of strings to prevent invalid URLs
