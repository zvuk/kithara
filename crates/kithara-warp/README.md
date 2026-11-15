<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-warp.svg)](https://crates.io/crates/kithara-warp)
[![docs.rs](https://docs.rs/kithara-warp/badge.svg)](https://docs.rs/kithara-warp)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-warp

Tempo for a playing track. `Warp<S>` wraps a decoded source and
`WarpRenderer` renders it at a speed, a ramp, or a sequence of steps
(`SpeedCurve`), with or without key lock, through a `kithara-stretch` engine.
`WarpMap` maps a track's beat grid onto the session grid for beat sync.

Every rendered chunk reports exactly which source frames it came from
(`AudioChunkInfo::source_span`), and fractional phase survives block
boundaries, seeks, and engine changes. Engine latency is absorbed during
preparation, not by the consumer.

Enable at least one engine: `stretch-signalsmith`, `stretch-bungee`,
`stretch-glide`, or `stretch-identity`. The `render` feature adds the
renderer; beat-map geometry is available without it.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-warp) for ownership and dependency boundaries.
