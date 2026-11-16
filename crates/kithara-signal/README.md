<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-signal.svg)](https://crates.io/crates/kithara-signal)
[![docs.rs](https://docs.rs/kithara-signal/badge.svg)](https://docs.rs/kithara-signal)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-signal

Value types for decoded audio, shared by decoders, playback, and analysis:
format (`AudioSpec`), chunks and their timeline, frame and sample units,
sample/time conversion, frame-range coverage, and the session output axis.
`SourceSpan` keeps the exact source-to-output mapping through nested slices.
No I/O, no threads, no backend features.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-signal) for the ownership contract.
