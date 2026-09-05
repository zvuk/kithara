<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-file.svg)](https://crates.io/crates/kithara-file)
[![docs.rs](https://docs.rs/kithara-file/badge.svg)](https://docs.rs/kithara-file)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-file

Single-file media streaming (MP3, AAC, FLAC, ALAC, WAV ...). Implements `kithara_stream::StreamType` for use with `Stream<File<S>>`. Backed by a pull-driven peer registered with the shared `kithara_stream::dl::Downloader`, a `kithara-assets` `AssetStore<S>` for disk caching, and the same application-owned `PoolRegion<S>`. Supports both remote HTTP sources and direct local-file playback.

## Usage

```rust
use kithara_assets::AssetStore;
use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};
use kithara_file::{File, FileConfig, FileSrc};
use kithara_stream::Stream;

pool_schema! {
    AppPools {
        bytes: u8,
    }
}

let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
    .bytes(PoolConfig::builder().max_buffers(128).build())
    .build()?;
let store = AssetStore::builder(pools.clone()).build();

// Remote HTTP source
let config = FileConfig::for_src(FileSrc::Remote(url))
    .store(store.clone())
    .pools(pools.clone())
    .build();
let stream = Stream::<File<AppPools>>::new(config).await?;

// Local file source
let local = FileConfig::for_src(FileSrc::Local(path))
    .store(store)
    .pools(pools)
    .build();
let stream = Stream::<File<AppPools>>::new(local).await?;
```

`FileConfig<S>` is a [`bon`](https://crates.io/crates/bon) builder. Start with `FileConfig::for_src(src)`, set the required shared asset store with `.store(store)` and its matching region with `.pools(pools)`, then call `.build()`. Both values use the same schema `S`; cloned regions retain one shared hard budget. The same chain accepts non-default wiring such as downloader, cache discriminator, and cancel token, plus the streaming knobs — extension hint, event and reader channel capacities, tmp-claim poll interval, look-ahead cap — as setters of their own.

## Key Types

<table>

<tr><th>Item</th><th>Kind</th><th>Role</th></tr>

<tr><td><code>File&lt;S&gt;</code></td><td>struct (marker)</td><td>Zero-sized type implementing <code>StreamType</code> for one pool schema</td></tr>

<tr><td><code>FileConfig&lt;S&gt;</code></td><td>struct (bon-builder)</td><td>Source, event-bus, downloader, asset store, pool region, cancel token</td></tr>

<tr><td><code>FileSrc</code></td><td>enum</td><td><code>Local(PathBuf)</code> for direct disk playback, <code>Remote(Url)</code> for HTTP streaming</td></tr>

</table>

`FileSource` is the `StreamType::Source` associated type; it is exported through `kithara_stream::Stream<File<S>>` and is rarely constructed directly. `FilePeer`, `FileCoord`, and the rest of the orchestration types are internal.

Local sources (`FileSrc::Local`) open directly via `AssetStore` and skip all network activity; remote sources (`FileSrc::Remote`) download pull-driven through a `FilePeer` registered with the shared `Downloader`. See [CONTEXT.md](CONTEXT.md) for the architecture diagram and the local/remote contracts.

## Features

<table>

<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>

<tr><td><code>default</code></td><td>yes</td><td><code>client-reqwest</code> + <code>tls-rustls</code></td></tr>

<tr><td><code>perf</code></td><td>no</td><td>Hotpath instrumentation (also enables <code>kithara-net/perf</code>)</td></tr>

<tr><td><code>probe</code></td><td>no</td><td>Compatibility feature for probe-aware test macro expansions</td></tr>

<tr><td><code>client-reqwest</code></td><td>yes</td><td>Forward the reqwest HTTP backend to network-reaching deps</td></tr>

<tr><td><code>client-wreq</code></td><td>no</td><td>Forward the wreq HTTP backend to network-reaching deps</td></tr>

<tr><td><code>tls-rustls</code></td><td>yes</td><td>Forward rustls TLS selection to network-reaching deps</td></tr>

<tr><td><code>tls-native</code></td><td>no</td><td>Forward native TLS selection to network-reaching deps</td></tr>

</table>

## Integration

Depends on `kithara-stream` (Peer/Downloader, Source, byte-map/playhead types), `kithara-net` (HTTP), `kithara-assets` (disk cache via `AssetStore<S>`), `kithara-storage` (`StorageResource`), `kithara-events` (`FileEvent` via the shared `EventBus`). Composes with `kithara-audio` as `Audio<Stream<File<S>>>` inside the decode pipeline.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
