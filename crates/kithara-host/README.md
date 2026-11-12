<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-host.svg)](https://crates.io/crates/kithara-host)
[![docs.rs](https://docs.rs/kithara-host/badge.svg)](https://docs.rs/kithara-host)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-host

The output session. A `Host` owns the audio device (or an offline renderer),
the shared output graph that mixes every deck, the session transport, and the
master limiter.

## Usage

```rust
use kithara_host::{Host, HostConfig};
use kithara_queue::{Queue, QueueConfig};

let mut host = Host::new(HostConfig::builder().build())?;
let deck = host.insert(Queue::new(queue_config))?;
deck.play();
// Removing the deck or dropping the Host releases its resources.
host.remove(&deck)?;
```

`insert` takes ownership of a fully built player or queue and returns a
`HostOwned` control handle. The audio device opens when the first deck plays.

## Key Types

- `Host` — the session; `insert`, `remove`, settings, subscriptions.
- `HostConfig` — realtime (device) or, with the `offline` feature, offline
  session: block size, deck limits, limiter, starting settings.
- `HostOwned` — control handle of an inserted deck.
- `HostSettings` — sample rate, session tempo, metronome, and ducking; they
  change while the session runs.

## Features

- `offline` — render a fixed range of frames without an audio device
  (`kithara_output::OfflineRenderer`).
- `backend-cpal` — native output through CPAL.

## Integration

Players come from `kithara-play` and `kithara-queue`; `kithara-link` decorates
the Host to keep decks on one tempo. All decks share the Host's buffer-pool
schema.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-host) for detailed contracts, invariants, and internals.
