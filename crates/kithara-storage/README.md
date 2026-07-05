<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-storage.svg)](https://crates.io/crates/kithara-storage)
[![docs.rs](https://docs.rs/kithara-storage/badge.svg)](https://docs.rs/kithara-storage)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-storage

Storage primitives for kithara. Provides a unified `StorageResource` enum (`Mmap` | `Mem`) with random-access `read_at`/`write_at`, blocking `wait_range`, and convenience `read_into`/`write_all`. `MmapResource` is file-backed via `mmap-io`; `MemResource` is fully in-memory. `OpenMode` controls file access for mmap: `Auto` (default), `ReadWrite`, or `ReadOnly`.

## Usage

```rust
use kithara_storage::{MmapResource, MmapOptions, OpenMode, StorageResource};

// File-backed (mmap)
let mmap = MmapResource::open(cancel_token, MmapOptions {
    path: path.into(),
    initial_len: None,
    mode: OpenMode::Auto,
})?;
let resource = StorageResource::from(mmap);
resource.write_at(0, &data)?;
let outcome = resource.wait_range(0..1024)?;
```

## Key Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>ResourceRead</code> (sealed trait)</td><td>Consumer-facing read API: <code>read_at</code>, <code>wait_range</code>, <code>contains_range</code>, status, length, and path helpers</td></tr>
<tr><td><code>ResourceWriter</code> / <code>ResourceReader</code></td><td>Typestate handles; writers own <code>write_at</code>, <code>commit</code>, and <code>fail</code>, while readers are cloneable read-only views</td></tr>
<tr><td><code>StorageResource</code></td><td>Enum dispatching to <code>MmapResource</code> or <code>MemResource</code></td></tr>
<tr><td><code>OpenMode</code></td><td>Access mode: <code>Auto</code>, <code>ReadWrite</code>, or <code>ReadOnly</code></td></tr>
<tr><td><code>ResourceStatus</code></td><td><code>Active</code>, <code>Committed { final_len }</code>, <code>Failed(String)</code>, or <code>Cancelled</code></td></tr>
<tr><td><code>WaitOutcome</code></td><td><code>Ready</code>, <code>Eof</code>, or <code>Interrupted</code>; storage core returns ready/EOF/errors, and higher-level wrappers may surface interruption</td></tr>
<tr><td><code>Atomic&lt;R&gt;</code></td><td>Decorator for crash-safe writes via write-temp-rename</td></tr>
<tr><td><code>AvailabilityObserver</code> (trait)</td><td>Hook for downstream layers (e.g. <code>kithara-assets</code>) to observe coverage changes as new ranges land</td></tr>
<tr><td><code>Driver</code> / <code>DriverIo</code> (traits)</td><td>Backend abstraction; <code>Resource&lt;S, D&gt;</code> is parameterised by lifecycle phase and driver</td></tr>
</table>

## Features

- `client-reqwest` / `client-wreq` and `tls-rustls` / `tls-native` — forward network backend selection for standalone test-utils builds.
- `probe` — enables USDT probes for tracing.
- `mock` — enables generated mocks for tests.
- `perf` — enables `hotpath` instrumentation.

## Integration

Foundation layer for `kithara-assets`. Higher-level concerns (trees of resources, eviction, leases) are handled by `kithara-assets`.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
