<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-net.svg)](https://crates.io/crates/kithara-net)
[![Downloads](https://img.shields.io/crates/d/kithara-net.svg)](https://crates.io/crates/kithara-net)
[![docs.rs](https://docs.rs/kithara-net/badge.svg)](https://docs.rs/kithara-net)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-net

HTTP networking with retry, timeout, and streaming support. Provides the `Net` trait for HTTP operations and `HttpClient` as the default reqwest-based implementation. Includes `TimeoutNet` decorator and `MockNet` for testing.

## Usage

```rust
use kithara_net::{HttpClient, Net, NetOptions};

let client = HttpClient::new()?;
let bytes = client.get_bytes(url, &NetOptions::default()).await?;
let stream = client.stream(url, &NetOptions::default()).await?;
```

## Decorators

| Decorator | Behavior |
|-----------|----------|
| `TimeoutNet<N>` | Wraps all methods with `tokio::time::timeout` |
| `RetryNet<N, P>` | Exponential backoff retry; retries on 5xx, 429, 408, timeouts; does not retry on other 4xx |

Decorators compose via `NetExt` extension trait:
```rust
HttpClient::new(opts).with_retry(policy, cancel).with_timeout(duration)
```

## Key Types

| Type | Role |
|------|------|
| `Net` (trait) | HTTP operations: `get_bytes`, `stream`, `get_range`, `head` |
| `HttpClient` | `reqwest::Client` wrapper implementing `Net` |
| `Headers` | `HashMap<String, String>` wrapper |
| `RangeSpec` | HTTP byte range: `{ start: u64, end: Option<u64> }` |
| `RetryPolicy` | Retry configuration: base delay, max delay, max retries |
| `NetError` | Error variants: `Http`, `Timeout`, `RetryExhausted`, `HttpError`, `Cancelled` |

## Timeout Behavior

- `get_bytes()` and `head()`: apply `request_timeout` from options.
- `stream()`: **no timeout** (designed for long-running downloads).
- The `TimeoutNet` decorator can override with a custom timeout.

## Integration

Used by `kithara-file` and `kithara-hls` for all HTTP operations. `MockNet` (behind `test-utils` feature) enables deterministic testing without network access.
