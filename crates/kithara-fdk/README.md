<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-fdk.svg)](https://crates.io/crates/kithara-fdk)
[![docs.rs](https://docs.rs/kithara-fdk/badge.svg)](https://docs.rs/kithara-fdk)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-fdk

Safe ownership and decoding operations for native FDK AAC, including its delayed
PCM flush. `Decoder` exclusively owns the native handle; callers supply raw AAC
packets and interleaved PCM storage. `StreamInfo` re-exports native metadata.

Codec registration, delay budgets, geometry validation, and Symphonia error
mapping remain in `kithara-decode`.

[Crate contracts](https://github.com/zvuk/kithara/wiki/kithara-decode).
