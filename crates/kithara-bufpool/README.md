<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-bufpool.svg)](https://crates.io/crates/kithara-bufpool)
[![docs.rs](https://docs.rs/kithara-bufpool/badge.svg)](https://docs.rs/kithara-bufpool)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-bufpool

Typed sharded buffers behind one cloneable region facade. A composition root
declares a closed schema, configures every pool explicitly, and shares one hard
byte budget across built-in byte/sample buffers and registered vector or string
keys.

## Usage

```rust
use kithara_bufpool::{
    OverallBudget, PoolAlias, PoolConfig, PoolError, StringKey, VecKey, pool_schema,
};

enum CommandsTag {}
enum TextTag {}

type Commands = PoolAlias<CommandsTag, VecKey<u32, 1>>;
type Text = PoolAlias<TextTag, StringKey<1>>;

pool_schema! {
    pub AppPools {
        bytes: u8,
        samples: f32,
        commands: Commands,
        text: Text,
    }
}

fn build() -> Result<(), PoolError> {
    let config = || PoolConfig::builder().max_buffers(32).build();
    let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
        .bytes(config())
        .samples(config())
        .commands(config())
        .text(config())
        .build()?;

    let mut samples = pools.get_with_len::<f32>(1024)?;
    samples.fill(0.0);
    let mut commands = pools.get::<Commands>();
    commands.try_push(7)?;
    let mut text = pools.get::<Text>();
    text.try_push_str("ready")?;
    let _bytes = pools.get::<u8>();
    Ok(())
}
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>PoolRegion&lt;S&gt;</code></td><td>Cloneable facade over one closed schema and shared hard budget</td></tr>

<tr><td><code>pool_schema!</code></td><td>Declares the registered keys and their typestate builder</td></tr>

<tr><td><code>PoolConfig</code></td><td>Retention, initial allocation, trim, and per-pool share policy</td></tr>

<tr><td><code>ByteBuffer</code> / <code>SampleBuffer</code></td><td>Checked RAII guards returned to their typed pool on drop</td></tr>

<tr><td><code>BufferRing&lt;B&gt;</code></td><td>Allocation-free FIFO indices over an owning fixed-size buffer</td></tr>

<tr><td><code>VecKey</code> / <code>StringKey</code></td><td>Safe registered storage shapes for crate-owned aliases</td></tr>

<tr><td><code>PooledVec</code> / <code>PooledString</code></td><td>Checked guards for registered vector and UTF-8 storage</td></tr>

<tr><td><code>OverallBudget</code> / <code>Percent</code></td><td>Region hard cap and an optional per-pool hard ceiling</td></tr>

<tr><td><code>PoolError</code></td><td>Typed construction, capacity, allocation, and budget failure</td></tr>

</table>

## Features

- `perf` — enables `hotpath` instrumentation on pool hot paths.
- `test-utils` — exposes the application-shaped `testing::TestPools` schema for
  workspace test harnesses.

## Integration

The app and FFI composition roots own concrete schemas. Lower layers are
generic over only the `HasPool<u8>` and `HasPool<f32>` capabilities they use.
Acquisition and return stay lock-free; every capacity increase is checked
against both the region and selected-pool limits.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
