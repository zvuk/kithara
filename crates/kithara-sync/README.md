<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-sync.svg)](https://crates.io/crates/kithara-sync)
[![docs.rs](https://docs.rs/kithara-sync/badge.svg)](https://docs.rs/kithara-sync)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-sync

Pure synchronization mathematics: sided entry placement, grid coverage, phase
error, bounded speed correction, and continuous piecewise-constant tempo
trajectories. It owns no player, renderer, group topology, or receipt custody.

## Usage

Construct a `TempoTrajectory` from a `TempoStep`, meter, and output sample rate.
Use `entry(&trajectory, &grid, position, Bound::AtOrAfter(frame))` or
`Bound::AtOrBefore(frame)` to find the nearest in-phase entry while preserving
the media position, expressed as `kithara_platform::time::Duration`. Use `covers` before
`phase_error`, and `speed` for the host-to-track tempo ratio.

See the [Sync contract](https://github.com/zvuk/kithara/wiki/kithara-sync).
