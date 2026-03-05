<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-test-utils

Shared test utilities for the kithara workspace. Provides fixtures, deterministic data generators, fixture server protocol, and local test helpers used by integration tests and benchmarks.

This crate is test-only infrastructure (`publish = false`), not part of runtime playback APIs.

## Usage

```rust
use kithara_test_utils::{create_test_wav, TestHttpServer, Xorshift64};

// Deterministic audio fixture
let wav = create_test_wav(4096, 44_100, 2);

// Deterministic PRNG for stress scenarios
let mut rng = Xorshift64::new(0xDEADBEEF);
let pos = rng.range_u64(0, 10_000);

// Local HTTP fixture server for tests (native only)
let server = TestHttpServer::new(router).await;
let url = server.url("/master.m3u8");
```

## Modules

<table>
<tr><th>Module</th><th>Platform</th><th>Purpose</th></tr>
<tr><td><code>fixtures</code></td><td>cross-platform</td><td>Reusable <code>rstest</code> fixtures (<code>temp_dir</code>, cancel tokens, tracing setup)</td></tr>
<tr><td><code>fixture_protocol</code></td><td>cross-platform</td><td>Serializable session configs (<code>HlsSessionConfig</code>, <code>AbrSessionConfig</code>), data generation functions (<code>generate_segment</code>, <code>expected_byte_at_test_pattern</code>), delay rules</td></tr>
<tr><td><code>fixture_client</code></td><td>cross-platform</td><td>HTTP client for fixture server sessions (<code>create_hls_session</code>, <code>create_abr_session</code>, <code>delete_session</code>)</td></tr>
<tr><td><code>http_server</code></td><td>native only</td><td><code>TestHttpServer</code> wrapper over Axum bound to random localhost port</td></tr>
<tr><td><code>memory_source</code></td><td>cross-platform</td><td>In-memory <code>Source</code> implementations for stream/read+seek tests</td></tr>
<tr><td><code>rng</code></td><td>cross-platform</td><td>Deterministic <code>Xorshift64</code> generator for reproducible stress tests</td></tr>
<tr><td><code>wav</code></td><td>cross-platform</td><td>WAV fixture generators: <code>create_test_wav</code>, <code>create_saw_wav</code></td></tr>
</table>

## Fixture Server Protocol

The `fixture_protocol` module defines serializable types shared between the fixture server (native binary) and fixture clients (both native and WASM):

- **Session configs**: `HlsSessionConfig`, `AbrSessionConfig`, `FixedHlsSessionConfig`
- **Data modes**: `DataMode::TestPattern`, `DataMode::SawWav`, `DataMode::PerVariantPcm`
- **Init modes**: `InitMode::None`, `InitMode::WavHeader`
- **Delay rules**: `DelayRule` with `variant`, `segment_eq`, `segment_gte`, `delay_ms`
- **Encryption**: `EncryptionRequest` for AES-128 HLS testing
- **Data generation**: Pure functions for segment/WAV data — shared between server and client for byte-level verification

The `fixture_client` module provides async functions to create/delete sessions via HTTP, used by WASM fixture wrappers.

## Integration

Used by `tests/` integration crate and benchmark targets to keep fixtures centralized and deterministic. On WASM, fixture protocol types enable cross-platform test infrastructure via an external fixture server.
