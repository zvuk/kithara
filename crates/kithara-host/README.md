<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-host.svg)](https://crates.io/crates/kithara-host)
[![docs.rs](https://docs.rs/kithara-host/badge.svg)](https://docs.rs/kithara-host)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-host

`kithara-host` owns Kithara's multi-player session, shared Firewheel output
graph, session transport, and platform audio backend. A player/deck remains in
`kithara-play`; pure synchronization values and laws belong to `kithara-sync`,
while beat-map projection and rendering belong to `kithara-warp`.

Callers fully construct a player or decorator and transfer that instance with
`Host::insert`. The Host attaches its opaque session capability exactly once
and retains the instance; callers receive a typed `HostOwned` control endpoint,
not the player value or its session dispatcher. Insertion registers deck controls
and validates known output geometry without opening an audio device. Playback
starts the output graph and allocates its slot, validating the actual output
geometry before rendering. Remove the deck or close the Host to release its
resources.

`Host<S>` shares the player's closed buffer-pool schema. Insertion accepts only
players with that same schema, while each registered deck retains its existing
`PoolRegion<S>` handle for graph-node scratch allocation.

`HostConfig<S>` directly selects and configures a realtime or offline session.
With the `offline` feature, the same `Host<S>` drives its owned graph without an
audio device and implements `kithara_output::OfflineRenderer` for exact finite
output-frame ranges. Its offline variant carries the pool, render quantum,
latency, worker, task, and dispatcher budgets.

The current crate is a mechanical ownership extraction. Runtime invariants and
dependency boundaries are documented in [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-host).
