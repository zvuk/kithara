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

<table>
<tr><th>Decorator</th><th>Behavior</th></tr>
<tr><td><code>TimeoutNet&lt;N&gt;</code></td><td>Wraps all methods with <code>tokio::time::timeout</code></td></tr>
<tr><td><code>RetryNet&lt;N, P&gt;</code></td><td>Exponential backoff retry; retries on 5xx, 429, 408, timeouts; does not retry on other 4xx</td></tr>
</table>

Decorators compose via `NetExt` extension trait:
```rust
HttpClient::new(opts).with_retry(policy, cancel).with_timeout(duration)
```

## Key Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>Net</code> (trait)</td><td>HTTP operations: <code>get_bytes</code>, <code>stream</code>, <code>get_range</code>, <code>head</code></td></tr>
<tr><td><code>HttpClient</code></td><td><code>reqwest::Client</code> wrapper implementing <code>Net</code></td></tr>
<tr><td><code>Headers</code></td><td><code>HashMap&lt;String, String&gt;</code> wrapper</td></tr>
<tr><td><code>RangeSpec</code></td><td>HTTP byte range: <code>{ start: u64, end: Option&lt;u64&gt; }</code></td></tr>
<tr><td><code>RetryPolicy</code></td><td>Retry configuration: base delay, max delay, max retries</td></tr>
<tr><td><code>NetError</code></td><td>Error variants: <code>Http</code>, <code>Timeout</code>, <code>RetryExhausted</code>, <code>HttpError</code>, <code>Cancelled</code></td></tr>
</table>

## Timeout Behavior

- `get_bytes()` and `head()`: apply `request_timeout` from options.
- `stream()`: **no timeout** (designed for long-running downloads).
- The `TimeoutNet` decorator can override with a custom timeout.

## Integration

Used by `kithara-file` and `kithara-hls` for all HTTP operations. `MockNet` (behind `test-utils` feature) enables deterministic testing without network access.
