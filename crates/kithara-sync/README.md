<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-sync.svg)](https://crates.io/crates/kithara-sync)
[![docs.rs](https://docs.rs/kithara-sync/badge.svg)](https://docs.rs/kithara-sync)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-sync

`kithara-sync` owns domain contracts for executing prepared musical
synchronization decisions.

The crate carries immutable execution identity, installed member alignment,
and domain rejection. It does not own tracks, decoder or seek epochs, worker
generation, resident mappings, rendering, queue policy, or presentation
acknowledgement. Those responsibilities remain in their Play and Warp owners.

See [crate contracts](https://github.com/zvuk/kithara/wiki/Crates) for the
ownership and dependency boundaries.
