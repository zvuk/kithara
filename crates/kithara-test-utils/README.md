<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-test-utils

Shared test utilities for the kithara workspace. Provides fixtures, deterministic data generators, and local test helpers used by integration tests and benchmarks.

This crate is test-only infrastructure (`publish = false`), not part of runtime playback APIs.

## Usage

```rust
use kithara_test_utils::{create_test_wav, TestHttpServer, Xorshift64};

// Deterministic audio fixture
let wav = create_test_wav(4096, 44_100, 2);

// Deterministic PRNG for stress scenarios
let mut rng = Xorshift64::new(0xDEADBEEF);
let pos = rng.range_u64(0, 10_000);

// Local HTTP fixture server for tests
let server = TestHttpServer::new(router).await;
let url = server.url("/master.m3u8");
```

## Modules

<table>
<tr><th>Module</th><th>Purpose</th></tr>
<tr><td><code>fixtures</code></td><td>Reusable <code>rstest</code> fixtures (<code>temp_dir</code>, cancel tokens, tracing setup)</td></tr>
<tr><td><code>http_server</code></td><td><code>TestHttpServer</code> wrapper over Axum bound to random localhost port</td></tr>
<tr><td><code>memory_source</code></td><td>In-memory <code>Source</code> implementations for stream/read+seek tests</td></tr>
<tr><td><code>rng</code></td><td>Deterministic <code>Xorshift64</code> generator for reproducible stress tests</td></tr>
<tr><td><code>wav</code></td><td>WAV fixture generators: <code>create_test_wav</code>, <code>create_saw_wav</code></td></tr>
</table>

## Integration

Used by `tests/` integration crate and benchmark targets to keep fixtures centralized and deterministic.
