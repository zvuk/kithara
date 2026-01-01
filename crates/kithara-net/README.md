# kithara-net

HTTP client layer for the Kithara audio streaming ecosystem.

## Overview

`kithara-net` provides a layered HTTP client implementation built on `reqwest` with support for:
- Timeout handling
- Retry with exponential backoff
- Range requests
- Custom headers
- Streaming byte responses

## Architecture

The library follows a decorator pattern with composable layers:

### Core Trait: `Net`
```rust
pub trait Net: Send + Sync {
    async fn get_bytes(&self, url: Url) -> Result<Bytes, NetError>;
    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError>;
    async fn get_range(&self, url: Url, range: RangeSpec, headers: Option<Headers>) -> Result<ByteStream, NetError>;
}
```

### Layers

1. **Base Layer (`ReqwestNet`)**: Basic HTTP client using reqwest
2. **Timeout Layer (`TimeoutNet<N>`)**: Adds timeout to all operations
3. **Retry Layer (`RetryNet<N, P>`)**: Adds retry logic with exponential backoff
4. **Builder (`NetBuilder`)**: Convenience interface for composing layers

## Usage

### Simple Client (Legacy)
```rust
use kithara_net::{NetClient, NetOptions};

let client = NetClient::new(NetOptions::default())?;
let bytes = client.get_bytes(url).await?;
```

### Layered Client (Recommended)
```rust
use kithara_net::{create_default_client, NetBuilder};

// Default client with retry + timeout
let client = create_default_client()?;

// Custom configuration
let client = NetBuilder::new()
    .with_request_timeout(Duration::from_secs(30))
    .with_retry_policy(RetryPolicy::new(
        3,                           // max_retries
        Duration::from_millis(100),    // base_delay
        Duration::from_secs(5)         // max_delay
    ))
    .build()?;

let bytes = client.get_bytes(url).await?;
```

### Manual Layer Composition
```rust
use kithara_net::{ReqwestNet, NetExt};

let base = ReqwestNet::new()?;
let client = base
    .with_timeout(Duration::from_secs(10))
    .with_retry(retry_policy);
```

## Retry Semantics

The retry layer follows these rules:

**Retryable errors:**
- HTTP 5xx server errors (500, 502, 503, 504)
- HTTP 429 Too Many Requests
- HTTP 408 Request Timeout
- Network timeouts
- Connection errors
- Other network-related failures

**Non-retryable errors:**
- HTTP 4xx client errors (except 408, 429)
- Invalid range specifications
- Implementation errors

**Backoff strategy:**
- Exponential backoff: `delay = base_delay * 2^(attempt-1)`
- Capped at `max_delay`
- Zero delay for first attempt

**Contract-level guarantees:**
- All retry decisions are centralized in `NetError::is_retryable()`
- Retry logic is applied only before streaming begins (mid-stream retries are not supported in v1)
- Classification respects HTTP status codes and network error patterns consistently across layers

## Timeout Semantics

The timeout layer applies timeouts as follows:
- `get_bytes()`: Timeout for entire operation
- `stream()` and `get_range()`: Timeout for request/response phase only
  (Not applied to the entire stream to avoid interrupting active downloads)

**Contract-level guarantees:**
- All timeout creation is centralized in `NetError::timeout()` for consistency
- Timeout errors are distinguishable via `NetError::is_timeout()`
- Timeout applies to connection establishment and HTTP response headers, not to ongoing stream data transfer

## Error Handling

All operations return `Result<T, NetError>` where `NetError` includes:

- `Http(String)`: HTTP-related errors with details
- `Timeout`: Operations exceeded timeout
- `RetryExhausted`: All retry attempts failed
- `HttpStatus { status, url }`: Non-successful HTTP status codes
- `InvalidRange(String)`: Malformed range specifications
- `Unimplemented`: Feature not yet available

## Testing

The crate includes comprehensive tests with a local HTTP server covering:
- Basic GET operations
- Range requests
- Header passthrough
- HTTP error handling
- Timeout behavior
- Retry policy logic
- Builder composition

Run tests with:
```bash
cargo test -p kithara-net
```