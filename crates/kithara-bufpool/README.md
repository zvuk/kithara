<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-bufpool.svg)](https://crates.io/crates/kithara-bufpool)
[![docs.rs](https://docs.rs/kithara-bufpool/badge.svg)](https://docs.rs/kithara-bufpool)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-bufpool

Generic sharded buffer pool for zero-allocation hot paths. Provides thread-safe, RAII-guarded buffers that automatically return to the pool on drop. Supports any type implementing the `Reuse` trait (`Vec<u8>`, `Vec<f32>`, etc.).

## Usage

```rust
use kithara_bufpool::{BytePool, PcmPool};

// Use the default singleton — typically constructed once at the top of the
// app and injected through your config structs. Library code should read
// the pool from injected config rather than calling `default()` itself.
let pool = PcmPool::default();
let mut buf = pool.get();
buf.resize(1024, 0.0);
// `buf` returns to the pool on drop.

let bytes = BytePool::default();
let mut chunk = bytes.get();
chunk.resize(4096, 0);
```

## Public Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>BytePool</code></td><td>Sharded pool of <code>Vec&lt;u8&gt;</code> for I/O and segment buffers</td></tr>
<tr><td><code>PcmPool</code></td><td>Sharded pool of <code>Vec&lt;f32&gt;</code> for decoded PCM frames</td></tr>
<tr><td><code>PcmBuf</code></td><td>RAII handle for a pooled PCM buffer</td></tr>
<tr><td><code>ByteBudget</code></td><td>Soft cap on outstanding bytes; returns <code>BudgetExhausted</code> when exceeded</td></tr>
<tr><td><code>BudgetExhausted</code></td><td>Error returned when a budgeted allocation would breach <code>ByteBudget</code></td></tr>
</table>

Advanced pool types are exported for workspace-internal integrations, but most callers use `BytePool`, `PcmPool`, and their RAII handles.

## Role in the workspace

The audio, decode, stream, storage, and network layers receive pools through their configs so each surface owns its sizing policy. `get`/`put` stay lock-free on the hot path.

## Features

- `perf` — enables `hotpath` instrumentation on pool hot paths.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
