<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-android.svg)](https://crates.io/crates/kithara-android)
[![docs.rs](https://docs.rs/kithara-android/badge.svg)](https://docs.rs/kithara-android)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-android

Android platform ABI and safe wrappers shared by Kithara crates.

This crate owns the raw NDK media binding surface, access to the host
runtime handle, and the host application's HTTP transport. Higher-level crates
use its typed wrappers instead of declaring local Android FFI structs, externs,
or binding dependencies, and they reach the Java runtime through it instead of
the platform global. The transport the application installs through
`com.kithara.net` becomes the process's `kithara-net` `HostTransport`. Codec policy remains in `kithara-decode`; the
player's `Java_*` entry points remain in `kithara-ffi`.

## Integration

[Crate contracts](https://github.com/zvuk/kithara/wiki/kithara-android).
