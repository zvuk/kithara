<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-download.svg)](https://crates.io/crates/kithara-download)
[![docs.rs](https://docs.rs/kithara-download/badge.svg)](https://docs.rs/kithara-download)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-download

Protocol-agnostic HTTP download scheduling shared by File and HLS. Owns the
request queue, concurrency limits, priorities, peer lifetime, and download events.
HTTP transport remains in `kithara-net`; protocol policy and storage stay with
callers. This crate and its tests do not depend on stream, File/HLS, or the facade.

## Usage

```rust
use kithara_download::{Downloader, DownloaderConfig};

// Reuse an application-owned HTTP client and register protocol peers.
let downloader = Downloader::new(DownloaderConfig::for_client(client).build());
let handle = downloader.register(peer);
```

The facade exposes the same API as `kithara::download` behind its `download`
feature. `client-reqwest` and `tls-rustls` are the crate defaults; `client-wreq`,
`client-apple`, and `tls-native` select the other supported transports.
`flash`, `no-block`, and `usdt` support isolated runtime validation.

## Integration

```sh
just test run -p kithara-download --flash=off
just test run -p kithara-download --flash=on --no-block=on
```

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-download)
for ownership and lifecycle invariants.
