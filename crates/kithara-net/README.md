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
use kithara_platform::CancelToken;

let client = HttpClient::new(NetOptions::default(), CancelToken::never());
let bytes = client.get_bytes(url, None).await?;        // None = no extra headers
let created = client.post_bytes(url, body, None).await?; // POST bytes, read the response body
let stream = client.stream(url, None).await?;
```

Decorators compose via the `NetExt` extension trait: `with_retry` adds exponential-backoff retry, `with_timeout` wraps every call in `TimeoutNet<N>`.

## Key Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>Net</code> (trait)</td><td>HTTP operations: <code>get_bytes</code>, <code>post_bytes</code>, <code>stream</code>, <code>get_range</code>, <code>head</code></td></tr>
<tr><td><code>HttpClient</code></td><td><code>reqwest::Client</code> wrapper implementing <code>Net</code></td></tr>
<tr><td><code>Headers</code></td><td><code>HashMap&lt;String, String&gt;</code> wrapper</td></tr>
<tr><td><code>RangeSpec</code></td><td>HTTP byte range: <code>{ start: u64, end: Option&lt;u64&gt; }</code></td></tr>
<tr><td><code>RetryPolicy</code></td><td>Retry configuration: base delay, max delay, max retries</td></tr>
<tr><td><code>NetError</code></td><td>Error variants: <code>Status</code>, <code>Timeout</code>, <code>Network</code>, <code>Decode</code>, <code>RetryExhausted</code>, <code>Unimplemented</code>, <code>Cancelled</code>, <code>InvalidContentType</code>. Retry decisioning via <code>retryability() -&gt; Retryability</code> (typed, no substring matching)</td></tr>
</table>

## Integration

Used by `kithara-file` and `kithara-hls` for all HTTP operations. `MockNet` (behind the `mock` feature) enables deterministic testing without network access.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
