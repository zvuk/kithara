<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-net.svg)](https://crates.io/crates/kithara-net)
[![docs.rs](https://docs.rs/kithara-net/badge.svg)](https://docs.rs/kithara-net)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-APACHE)

</div>

# kithara-net

HTTP client with retry, timeout, and streaming. Wraps the selected backend (`reqwest` by default, `wreq`, or Apple `NSURLSession`) behind the `Net` trait, with `NetMock` for tests.

## Usage

```rust
use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};
use kithara_net::{HttpClient, Net, NetOptions};
use kithara_platform::CancelToken;

pool_schema! {
    NetPools {
        bytes: u8,
    }
}

let pools = NetPools::builder(OverallBudget(64 * 1024 * 1024))
    .bytes(PoolConfig::builder().max_buffers(32).build())
    .build()?;
let client = HttpClient::new(NetOptions::default(), pools, CancelToken::never());
let bytes = client.get_bytes(url, None).await?;        // None = no extra headers
let created = client.post_bytes(url, body, None).await?; // POST bytes, read the response body
let stream = client.stream(url, None).await?;
```

Retry and the inactivity timeout come from `NetOptions`. `with_retry_policy` returns a handle with a different retry policy that shares the connection pool.

The inactivity timeout bounds response headers and each body-chunk wait separately on every backend. `HttpClient::new_with_transport` accepts an instance implementing `HttpTransport`; supplied transports use the same client policies and remain shared by derived handles.

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Net</code> (trait)</td><td>HTTP operations: <code>get_bytes</code>, <code>post_bytes</code>, <code>stream</code>, <code>get_range</code>, <code>head</code></td></tr>

<tr><td><code>HttpClient</code></td><td>Backend-agnostic client implementing <code>Net</code> for the selected feature backend</td></tr>

<tr><td><code>Headers</code></td><td><code>HashMap&lt;String, String&gt;</code> wrapper</td></tr>

<tr><td><code>RangeSpec</code></td><td>HTTP byte range: <code>{ start: u64, end: Option&lt;u64&gt; }</code></td></tr>

<tr><td><code>RetryPolicy</code></td><td>Retry configuration: base delay, max delay, max retries</td></tr>

<tr><td><code>NetError</code></td><td>Error variants: <code>Status</code>, <code>Timeout</code>, <code>Network</code>, <code>Decode</code>, <code>RetryExhausted</code>, <code>Cancelled</code>, <code>InvalidContentType</code>. Retry decisioning via <code>retryability() -&gt; Retryability</code> (typed, no substring matching)</td></tr>

</table>

## Features

- `client-reqwest` (default) — portable reqwest backend, required on wasm32.
- `client-wreq` — native wreq backend with BoringSSL and browser fingerprint emulation; shadows reqwest when both are enabled.
- `client-apple` — macOS/iOS `NSURLSession` backend; shadows the reqwest/wreq seam on Apple platforms when enabled.
- `tls-rustls` (default) / `tls-native` — TLS selection for the reqwest path only.
- `mock` — `NetMock`, a deterministic client for tests that must not reach the network (native only).
- `usdt` — USDT probes for tracing.
- `perf` — enables `hotpath` instrumentation.

## Integration

Used by `kithara-file` and `kithara-hls` for all HTTP operations.

The native Safari18 and Chrome137 profiles and their TLS certificate codecs derive from Apache-2.0 licensed wreq-util 3.0.0-rc.14. See [NOTICE](./NOTICE) for attribution and modifications.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-net) for detailed contracts, invariants, and internals.
