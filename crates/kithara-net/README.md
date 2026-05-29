<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-net.svg)](https://crates.io/crates/kithara-net)
[![docs.rs](https://docs.rs/kithara-net/badge.svg)](https://docs.rs/kithara-net)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-net

HTTP client with retry, timeout, and streaming. Wraps reqwest behind the `Net` trait, with a `TimeoutNet` decorator and a `MockNet` for tests.

## Usage

```rust
use kithara_net::{HttpClient, Net, NetOptions};
use tokio_util::sync::CancellationToken;

let client = HttpClient::new(NetOptions::default(), CancellationToken::new());
let bytes = client.get_bytes(url, None).await?;        // None = no extra headers
let stream = client.stream(url, None).await?;
```

## Decorators

`TimeoutNet<N>` wraps all methods with `tokio::time::timeout` and is exported in the public API. A retry decorator with exponential backoff (retries on 5xx, 429, 408, timeouts; does not retry on other 4xx) is also available, but only via the `NetExt` builder methods — the wrapper type itself is not part of the public surface.

Decorators compose via the `NetExt` extension trait:
```rust
use kithara_net::{HttpClient, Net, NetExt, NetOptions, RetryPolicy};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

let client = HttpClient::new(NetOptions::default(), CancellationToken::new())
    .with_retry(RetryPolicy::default(), CancellationToken::new())
    .with_timeout(Duration::from_secs(30));
```

## Key Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>Net</code> (trait)</td><td>HTTP operations: <code>get_bytes</code>, <code>stream</code>, <code>get_range</code>, <code>head</code></td></tr>
<tr><td><code>HttpClient</code></td><td><code>reqwest::Client</code> wrapper implementing <code>Net</code></td></tr>
<tr><td><code>Headers</code></td><td><code>HashMap&lt;String, String&gt;</code> wrapper</td></tr>
<tr><td><code>RangeSpec</code></td><td>HTTP byte range: <code>{ start: u64, end: Option&lt;u64&gt; }</code></td></tr>
<tr><td><code>RetryPolicy</code></td><td>Retry configuration: base delay, max delay, max retries</td></tr>
<tr><td><code>NetError</code></td><td>Error variants: <code>Http</code>, <code>Timeout</code>, <code>RetryExhausted</code>, <code>HttpError</code>, <code>Unimplemented</code>, <code>Cancelled</code></td></tr>
</table>

## Timeout Behavior

Two independent limits in `NetOptions`, applied to **all** methods (`get_bytes`,
`head`, `get_range`, `stream`):

- `inactivity_timeout` (default 30s) — max gap between reads (reqwest
  `read_timeout`); guards against stalled connections, not total duration.
- `total_timeout` (default 120s) — hard cap on request lifetime. Set to `None`
  to allow indefinite streaming as long as data keeps flowing.

The `TimeoutNet` decorator can wrap any `Net` with an additional
`tokio::time::timeout` over the whole call.

## Trait Bridges

- `&RangeSpec` → `String` (`Display`) — HTTP Range header rendering
- `HashMap<String, String>` → `Headers` (`From`) — build header set from a map
- `Compression` → `Vec<ClientBuilderMod>` (`From`) — map compression flags to reqwest builder mods
- `ReqwestError` → `NetError` (`From`) — wrap transport errors into typed `NetError`

## Integration

Used by `kithara-file` and `kithara-hls` for all HTTP operations. `MockNet` (behind the `mock` feature) enables deterministic testing without network access.
