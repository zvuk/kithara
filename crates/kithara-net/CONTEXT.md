# kithara-net — Context

Detailed contracts and invariants for the kithara-net crate; the README is the overview.

## HTTP client backends

The HTTP transport is selected by Cargo features. Exactly one client backend is
active per target; the choice is invisible above the `Net` trait — `HttpClient`,
`NetOptions`, and `NetError` are backend-agnostic.

| Feature | Client | TLS | Targets | Notes |
|---|---|---|---|---|
| `client-reqwest` (**default**) | `reqwest` | `tls-rustls` (default) / `tls-native` | native + wasm | Pure-Rust, portable. The only backend on wasm32. |
| `client-wreq` | `wreq` | `BoringSSL` (fixed) | native only | Browser TLS/HTTP2 emulation (`ImpersonatePreset`) to defeat anti-bot WAF JA3 fingerprinting. |

Rules:

- **Exactly one backend.** A `compile_error!` (in `lib.rs`) fires if no backend is
  selected for the target. This is a contract, not a fallback — there is no silent
  default-to-reqwest when a misconfigured build drops every client feature.
- **`client-wreq` wins when both unify.** Cargo features are additive, so a build
  that pulls both (e.g. the Apple/Android SDK) compiles both crates but the code
  picks `wreq` via `cfg` priority (`cfg(all(not(wasm32), feature = "client-wreq"))`).
- **wasm32 is always `client-reqwest`.** `wreq`/BoringSSL has no wasm target, so
  the `client-wreq` dep is gated to `cfg(not(wasm32))` and the wasm guard requires
  `client-reqwest`. TLS features are inert on wasm (the browser owns TLS).
- **TLS axis applies only to `client-reqwest`.** `tls-rustls` / `tls-native` map to
  `reqwest`'s `rustls` / `native-tls`; they are no-ops under `client-wreq` (always
  BoringSSL) and on wasm (reqwest gates its TLS deps to `cfg(not(wasm32))`).

Why reqwest is the default and wreq is opt-in: Cargo feature unification makes
"disable a transitive default" effectively impossible across the dependency graph,
while "add a forwarded feature" composes cleanly. So the special backend (`wreq`,
which pulls BoringSSL — a C toolchain not every open-source consumer wants) must be
opt-in, and the portable one (`reqwest`) the default. Device SDK builds opt in via
the `kithara` facade's `apple` / `android` features (`kithara-net?/client-wreq`).

## Decorators

`TimeoutNet<N>` wraps all methods with `tokio::time::timeout` and is exported in the public API. A retry decorator with exponential backoff (retries on 5xx, 429, 408, timeouts; does not retry on other 4xx) is also available, but only via the `NetExt` builder methods — the wrapper type itself is not part of the public surface.

Decorators compose via the `NetExt` extension trait:
```rust
use kithara_net::{HttpClient, Net, NetExt, NetOptions, RetryPolicy};
use std::time::Duration;
use kithara_platform::CancelToken;

let client = HttpClient::new(NetOptions::default(), CancelToken::never())
    .with_retry(RetryPolicy::default(), CancelToken::never())
    .with_timeout(Duration::from_secs(30));
```

## Timeout Behavior

Two independent limits in `NetOptions`, applied to **all** methods (`get_bytes`,
`head`, `get_range`, `stream`):

- `inactivity_timeout` (default 30s) — max gap between reads (reqwest
  `read_timeout`); guards against stalled connections, not total duration.
- `total_timeout` (default 120s) — hard cap on request lifetime. Set to `None`
  to allow indefinite streaming as long as data keeps flowing.

The `TimeoutNet` decorator can wrap any `Net` with an additional
`tokio::time::timeout` over the whole call.

## Trait Bridges

- `&RangeSpec` → `String` (`Display`) — HTTP Range header rendering
- `HashMap<String, String>` → `Headers` (`From`) — build header set from a map
- `Compression` → `Vec<ClientBuilderMod>` (`From`) — map compression flags to native-backend (`wreq`/`reqwest`) builder mods
- `ImpersonatePreset` → `wreq_util::Emulation` (`From`) — only under `client-wreq`
- `ReqwestError` → `NetError` (`From`) — wrap transport errors into typed `NetError` (`ReqwestError` aliases the active backend's error type)
