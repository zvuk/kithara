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
derives a closed schema, configures every pool explicitly, and shares one hard
byte budget across built-in byte/sample buffers and registered vector or string
keys.

## Usage

```rust
use kithara_bufpool::{
    OverallBudget, Pool, PoolAlias, PoolConfig, PoolError, PoolSlot, StringKey, VecKey,
};

enum CommandsTag {}
enum TextTag {}

type Commands = PoolAlias<CommandsTag, VecKey<u32, 1>>;
type Text = PoolAlias<TextTag, StringKey<1>>;

#[derive(Pool)]
pub struct AppPools {
    bytes: PoolSlot<u8>,
    samples: PoolSlot<f32>,
    commands: PoolSlot<Commands>,
    text: PoolSlot<Text>,
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

<tr><td><code>#[derive(Pool)]</code></td><td>Turns a struct of private <code>PoolSlot&lt;K&gt;</code> fields into a closed schema with a module-private typestate builder</td></tr>

<tr><td><code>PoolSlot&lt;K&gt;</code></td><td>Opaque storage for one registered key; only the derived builder creates it</td></tr>

<tr><td><code>PoolConfig</code></td><td>Retention and initial allocation policy</td></tr>

<tr><td><code>ByteBuffer</code> / <code>SampleBuffer</code></td><td>Checked RAII guards returned to their typed pool on drop</td></tr>

<tr><td><code>RingProd</code> / <code>RingCons</code></td><td>Halves of an SPSC ring whose slots are one pooled buffer, from <code>PoolRegion::ring</code></td></tr>

<tr><td><code>VecKey</code> / <code>StringKey</code></td><td>Safe registered storage shapes for crate-owned aliases</td></tr>

<tr><td><code>PooledVec</code> / <code>PooledString</code></td><td>Checked guards for registered vector and UTF-8 storage</td></tr>

<tr><td><code>OverallBudget</code></td><td>Region hard cap shared by every pool</td></tr>

<tr><td><code>PoolError</code></td><td>Typed construction, capacity, allocation, and budget failure</td></tr>

</table>

## Features

- `perf` — enables `hotpath` instrumentation on pool hot paths.

## Integration

The app and FFI composition roots own concrete schemas. A crate deriving
`Pool` depends on `kithara-bufpool` directly, because the expansion names
`::kithara_bufpool` paths. Lower layers are
generic over only the `HasPool<u8>` and `HasPool<f32>` capabilities they use.
Acquisition and return stay lock-free; every capacity increase is checked
against the region limit.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-bufpool) for detailed contracts, invariants, and internals.
