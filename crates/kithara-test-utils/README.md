<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-test-utils

Shared test utilities for the kithara workspace: deterministic fixtures, synthetic HLS, procedural audio URLs, and a small HTTP test server. This crate is test-only (`publish = false`); it is not part of the public playback API.

## Test server

The **unified test server** is a local HTTP service used by integration tests. It never talks to the real internet: everything is generated or read from the repository.

### What it serves

| Path prefix | Purpose |
|-------------|---------|
| `/assets/...` | Static files from the repo (regression assets). |
| `/stream/...` | Synthetic HLS: master/media playlists, init segments, media segments, keys. The URL carries a **token** that selects the fixture spec. |
| `/signal/...` | Procedural audio (saw, sine, sweep, silence, …) as `wav` or encoded formats (`mp3`, `flac`, `aac`, `m4a` on native). Also token-backed. |
| `POST /token` | Registers a JSON spec and returns a UUID used inside `/stream` and `/signal` URLs. You rarely call this directly. |
| `GET /health` | Simple readiness probe for runners. |

**You do not hand-build tokens.** Helpers register the spec and give you normal `Url` values.

### Native integration tests (usual case)

Spawn the server **in the same process** on a random port and build fixtures with `TestServerHelper`:

```rust
use kithara_test_utils::{HlsFixtureBuilder, TestServerHelper};

let helper = TestServerHelper::new().await;
let hls = helper
    .create_hls(
        HlsFixtureBuilder::new()
            .variant_count(2)
            .segments_per_variant(6),
    )
    .await
    .expect("create HLS fixture");

let master = hls.master_url();
// Use `master` with the player under test; use `hls.media_url(i)`, `hls.segment_url(v, s)`, etc.
```

For procedural audio, use `helper.sawtooth(&spec).await`, `helper.sine(&spec, freq_hz).await`, `helper.sweep(&spec, start_hz, end_hz, mode).await`, and similar methods on `TestServerHelper`. For static assets, use `helper.asset("relative/path")`.

### Standalone process (WASM / debugging)

Run the binary from the integration-tests crate:

```bash
cargo run -p kithara-integration-tests --bin test_server
```

By default it listens on **`127.0.0.1:3444`**. Set **`TEST_SERVER_PORT`** to use another port.

Browser WASM tests typically use the same server; point the harness at **`TEST_SERVER_URL`** (default `http://127.0.0.1:3444`). See `tests/README.md` for the full WASM flow.

### Picking a higher-level API

| Use when | API |
|----------|-----|
| Quick canned multi-variant HLS | `hls_fixture::TestServer` |
| Custom variants, delays, encryption | `hls_fixture::HlsTestServer` / `HlsTestServerConfig` |
| ABR switching scenarios | `hls_fixture::AbrTestServer` |
| Real fMP4 audio (preferred for new audio HLS tests) | `HlsFixtureBuilder::packaged_audio_*` or `PackagedTestServer` |
| Legacy byte-exact compatibility | `TestServer`, `DataMode::AbrBinary`, `InitMode::TestInit` as needed |

Under the hood these all go through the same `/stream` contract and `TestServerHelper` (native) or the standalone server (WASM).

## Modules

<table>
<tr><th>Module</th><th>Platform</th><th>Role</th></tr>
<tr><td><code>fixtures</code></td><td>cross-platform</td><td>Shared <code>rstest</code> fixtures (<code>temp_dir</code>, cancel tokens, tracing)</td></tr>
<tr><td><code>fixture_protocol</code></td><td>cross-platform</td><td>Types and pure generators for synthetic HLS payloads</td></tr>
<tr><td><code>hls_fixture</code></td><td>cross-platform</td><td>Preset servers: <code>TestServer</code>, <code>HlsTestServer</code>, <code>AbrTestServer</code>, packaged helpers</td></tr>
<tr><td><code>http_server</code></td><td>native</td><td><code>TestHttpServer</code> — Axum on a random localhost port</td></tr>
<tr><td><code>memory_source</code></td><td>cross-platform</td><td>In-memory <code>Source</code> for read/seek tests</td></tr>
<tr><td><code>rng</code></td><td>cross-platform</td><td>Deterministic <code>Xorshift64</code></td></tr>
<tr><td><code>wav</code></td><td>cross-platform</td><td><code>create_test_wav</code>, <code>create_saw_wav</code></td></tr>
<tr><td><code>test_server</code></td><td>native impl; WASM uses remote server</td><td><code>TestServerHelper</code>, <code>HlsFixtureBuilder</code>, <code>run_test_server</code>, route wiring</td></tr>
</table>

## Native encoded `/signal` formats

Encoded outputs (`mp3`, `flac`, `aac`, `m4a`) use `ffmpeg-next` and need a system FFmpeg discoverable via `pkg-config` / `pkgconf` at build time.
