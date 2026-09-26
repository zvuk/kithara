<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-mpa.svg)](https://crates.io/crates/kithara-mpa)
[![docs.rs](https://docs.rs/kithara-mpa/badge.svg)](https://docs.rs/kithara-mpa)
[![License](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](LICENSE)

</div>

# kithara-mpa

MPEG audio (MP1/MP2/MP3) demuxer for Symphonia, forked so a packet read is a
byte-exact transaction over a non-blocking source.

This crate is a fork of `symphonia-bundle-mp3` 0.6.0's `MpaReader` (MPL-2.0,
see `LICENSE`). It carries no decoder: `MpaDecoder` comes from the upstream
crate through Symphonia's codec registry, unchanged.

Only the demuxer is forked: to add rollback on a transient read, and to
publish no duration for a headerless stream whose leading frames differ in
bitrate instead of extrapolating one. Everything else tracks upstream verbatim
so the delta stays reviewable.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-mpa) for the rollback and duration contracts, the fork's exact
delta, and how to rebase it onto a new upstream release.
