# `kithara-net` — networking primitives for Kithara

`kithara-net` provides HTTP networking abstractions for Kithara, with support for:
- Progressive downloads with range requests
- Retry logic with configurable policies
- Timeout handling
- Streaming response bodies
- Header manipulation and validation

This crate is designed to be generic over HTTP clients while providing a consistent interface
for higher-level crates like `kithara-file` and `kithara-hls`.

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core abstractions
- `trait Net` — Base networking trait for HTTP operations
- `trait NetExt` — Extension methods for common operations
- `trait ByteStream` — Stream of bytes from network responses

### Concrete implementations
- `struct HttpClient` — `reqwest`-based HTTP client implementation
- `struct TimeoutNet<N>` — Decorator that adds timeout handling to any `Net` implementation

### Configuration and types
- `struct NetOptions` — Configuration for network operations
- `struct RetryPolicy` — Configurable retry behavior
- `enum RangeSpec` — Range request specification (None, From, Full)
- `type Headers = http::HeaderMap` — HTTP headers type alias

### Error handling
- `enum NetError` — Error type for network operations
- `type NetResult<T> = Result<T, NetError>` — Result alias

## Core invariants

1. **Streaming responses**: Large responses are streamed, not buffered entirely in memory
2. **Range support**: All implementations support HTTP range requests for progressive downloads
3. **Cancellation**: All async operations support cancellation
4. **Retry semantics**: Retries follow configurable policies with exponential backoff
5. **Timeout handling**: Operations have configurable timeouts to prevent hangs
6. **Error classification**: Errors are categorized (Connect, Timeout, Http, etc.) for appropriate handling

## `Net` trait

The `Net` trait defines the core networking operations:

```rust
#[async_trait::async_trait]
pub trait Net: Send + Sync + 'static {
    async fn get(
        &self,
        url: &str,
        range: RangeSpec,
        headers: Headers,
        cancel: CancellationToken,
    ) -> NetResult<(Headers, Box<dyn ByteStream>)>;
}
```

Key aspects:
- Returns headers and a byte stream for progressive reading
- Supports range requests for partial content
- Accepts custom headers for authentication, content negotiation, etc.
- All operations are cancellable via `CancellationToken`

## `NetExt` trait

Provides convenience methods built on top of the `Net` trait:

- `get_full` — Download entire resource
- `get_range` — Download specific byte range
- `get_with_retry` — Download with retry logic
- `head` — HEAD request for metadata

## `ByteStream` trait

Async stream of bytes from network responses:

```rust
#[async_trait::async_trait]
pub trait ByteStream: Send + Sync {
    async fn next(&mut self, cancel: CancellationToken) -> NetResult<Option<Bytes>>;
}
```

Designed for:
- Progressive reading of large responses
- Integration with `kithara-stream` orchestration
- Efficient memory usage (stream chunks as they arrive)

## Error handling

`NetError` categorizes network failures:

- `Connect` — Connection establishment failed
- `Timeout` — Operation timed out
- `Http` — HTTP protocol error (4xx, 5xx)
- `InvalidUrl` — Malformed URL
- `Cancelled` — Operation was cancelled
- `Other` — Other network-related errors

Each variant includes context about the operation (URL, range, etc.) for debugging.

## Configuration

### `NetOptions`
- `connect_timeout: Duration` — Connection establishment timeout
- `read_timeout: Duration` — Read operation timeout
- `user_agent: Option<String>` — User-Agent header
- `max_redirects: usize` — Maximum redirects to follow
- `pool_idle_timeout: Duration` — Connection pool idle timeout

### `RetryPolicy`
- `max_attempts: usize` — Maximum retry attempts
- `initial_backoff: Duration` — Initial backoff duration
- `backoff_multiplier: f64` — Backoff multiplier for exponential backoff
- `max_backoff: Duration` — Maximum backoff duration
- `retry_on_timeout: bool` — Whether to retry on timeout
- `retry_on_connect: bool` — Whether to retry on connection errors

## Integration with other Kithara crates

### `kithara-file`
- Uses `Net` for progressive MP3 downloads
- Implements `EngineSource` using HTTP client
- Handles range requests for seeking

### `kithara-hls`
- Uses `Net` for playlist and segment downloads
- Implements retry logic for segment failures
- Uses timeout decorator for reliable streaming

### `kithara-stream`
- `EngineSource` implementations use `Net` for data fetching
- Byte streams integrate with `Writer` for storage
- Error propagation through `StreamError<NetError>`

## Example usage

### Basic HTTP GET
```rust
use kithara_net::{HttpClient, NetOptions, RangeSpec};
use tokio_util::sync::CancellationToken;

let options = NetOptions::default();
let client = HttpClient::new(options);
let cancel = CancellationToken::new();

let (headers, mut stream) = client.get(
    "https://example.com/audio.mp3",
    RangeSpec::None,
    Default::default(),
    cancel.clone(),
).await?;

// Stream the response
while let Some(chunk) = stream.next(cancel.clone()).await? {
    // Process chunk
}
```

### Range request with retry
```rust
use kithara_net::{HttpClient, NetExt, RangeSpec, RetryPolicy};
use std::time::Duration;

let client = HttpClient::default();
let cancel = CancellationToken::new();
let retry_policy = RetryPolicy {
    max_attempts: 3,
    initial_backoff: Duration::from_millis(100),
    backoff_multiplier: 2.0,
    max_backoff: Duration::from_secs(5),
    retry_on_timeout: true,
    retry_on_connect: true,
};

let (headers, stream) = client.get_with_retry(
    "https://example.com/large.mp3",
    RangeSpec::From(1024), // Start from byte 1024
    Default::default(),
    cancel.clone(),
    &retry_policy,
).await?;
```

### Timeout decorator
```rust
use kithara_net::{HttpClient, TimeoutNet, NetOptions};
use std::time::Duration;

let base_client = HttpClient::new(NetOptions::default());
let timeout_client = TimeoutNet::new(
    base_client,
    Duration::from_secs(10), // Connect timeout
    Duration::from_secs(30), // Read timeout
);

// Use timeout_client as a regular Net implementation
```

## Design philosophy

1. **Abstraction over implementation**: `Net` trait allows switching HTTP clients
2. **Composition over inheritance**: Decorators (TimeoutNet) add functionality
3. **Async-first**: All operations designed for async Rust
4. **Cancellation support**: All operations respect cancellation tokens
5. **Progressive loading**: Stream-based API for large resources
6. **Configurable policies**: Retry, timeout, and other behaviors are configurable