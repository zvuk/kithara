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
| `client-apple` | `NSURLSession` | Apple platform trust | macOS + iOS only | Native Apple backend for SDK size reduction. Objective-C FFI is isolated under `src/backend/apple/`. |

Rules:

- **Exactly one backend.** A `compile_error!` (in `backend/mod.rs`) fires if no backend
  is selected for the target. This is a contract, not a fallback — there is no silent
  default-to-reqwest when a misconfigured build drops every client feature.
- **Backend features are mutually exclusive.** Cargo features are additive, so a
  build that pulls two client backends is a configuration bug. The crate rejects
  that shape at compile time instead of picking a fallback backend.
- **wasm32 is always `client-reqwest`.** `wreq`/BoringSSL has no wasm target, so
  the `client-wreq` dep is gated to `cfg(not(wasm32))` and the wasm guard requires
  `client-reqwest`. `client-apple` is also native-only and rejected on wasm32.
  TLS features are inert on wasm (the browser owns TLS).
- **TLS axis applies only to `client-reqwest`.** `tls-rustls` / `tls-native` map to
  `reqwest`'s `rustls` / `native-tls`; they are no-ops under `client-wreq` (always
  BoringSSL), `client-apple` (platform trust), and on wasm (the browser owns TLS).

Why reqwest is the default and wreq is opt-in: Cargo feature unification makes
"disable a transitive default" effectively impossible across the dependency graph,
while "add a forwarded feature" composes cleanly. So the special backend (`wreq`,
which pulls BoringSSL — a C toolchain not every open-source consumer wants) must be
opt-in, and the portable one (`reqwest`) the default. Device SDK builds opt in via
the facade feature-forwarding chain; this crate only enforces the selected backend
once it arrives.

Backend layout:

- `client.rs` owns the shared `RawHttp` / `HttpClient` logic and consumes only
  the uniform `backend` seam. It has no backend selection logic.
- `backend/mod.rs` owns backend selection, compile-time guard failures, and the
  public `HttpClient` re-export.
- When `client-apple` is not selected, `backend/mod.rs` selects the active
  platform folder: `native/` for non-wasm targets and `wasm/` for wasm32.
- `backend/native/mod.rs` selects the active native backend (`reqwest` unless
  `client-wreq` is enabled), owns native-only compression builder
  transforms, and provides native `HEAD`.
- `backend/native/reqwest.rs` builds the native reqwest client.
- `backend/native/wreq.rs` builds the native wreq client and maps
  `ImpersonatePreset` to `wreq_util::Emulation`.
- `backend/wasm/fetch.rs` builds the wasm reqwest-fetch client and maps
  `head` to a one-byte ranged `GET`.
- `backend/apple/mod.rs` is declaration/re-export only for the Apple adapter.
- `backend/apple/client.rs` owns the safe `AppleNet` implementation for
  `client-apple`; it bypasses the reqwest/wreq backend seam because
  `NSURLSession` streams through a delegate rather than a Rust HTTP response type.
- `backend/apple/session.rs` configures `NSURLSession`, starts data/stream tasks,
  and owns task identity/resume/cancel handles.
- `backend/apple/delegate.rs` owns the Objective-C delegate class and callbacks.
- `backend/apple/request.rs` owns request construction, URL validation, header
  application, and accepted-content-encoding selection.
- `backend/apple/response.rs` owns HTTP response/header conversion, NSError
  mapping, and shared completion/terminal helpers.
- `backend/apple/stream.rs` owns body stream polling, cancel wake registration,
  and startup/data task guards.

`client-apple` timeout note: keep timeout ownership aligned with
`client-wreq`/`client-reqwest`. The Apple backend wraps data/header establishment
in the Rust-side `inactivity_timeout`, and streaming body inactivity is still
owned by `resumable_body`. `NSURLSession` only receives
`timeoutIntervalForResource` from `total_timeout` when present, so Foundation
does not race the flash-aware idle timers. Cancellation is still driven by
`CancelToken`: startup cancels the task directly, and the body stream holds a
cancel waker so a parked poll is woken promptly.

`AppleSession` owns exactly one Foundation `NSURLSession` per `HttpClient`.
Apple's public pooling controls are not isomorphic to reqwest/wreq: `NetOptions`
`pool_max_idle_per_host` is an idle-pool budget, while Foundation's
`HTTPMaximumConnectionsPerHost` is a cap on simultaneous persistent connections
per host. The Apple backend uses that documented Foundation control as the
closest per-host pool knob and sets it only when `pool_max_idle_per_host`
converts to a positive `NSInteger`; otherwise Foundation's default remains in
effect. Do not substitute `NSInteger::MAX` or another sentinel. Connection reuse
is managed by keeping all `head`, `get_bytes`, `stream`, and `get_range` tasks on
the same session so every `HttpClient` clone shares the same Foundation
connection pool. Do not split data and streaming requests into separate sessions
unless there is a new explicit pooling contract. `HttpClient::connection_count()`
is the client-level opened-connection counter used by shared-pool regression
tests: reqwest/wreq count successful native connector-layer opens through the
backend-owned metrics adapter, Apple counts
`NSURLSessionTaskMetrics.transactionMetrics` entries where Foundation reports
`isReusedConnection == false`, and wasm fetch reports zero because browsers do
not expose socket creation. Server-side TCP instrumentation, if needed for a
specific test, belongs in test-server support rather than downloader or stream
production code.

When `client-apple` is not selected, the seam re-exported by `backend` is exactly
`Client`, `RequestBuilder`, `Response`, `BackendError`,
`build_client(&NetOptions, &ConnectionMetrics)`, and
`head_request(&Client, Url)`. `ClientBuilder`, compression transforms, and
native connector metrics stay under `backend/native/`; shared HTTP code must not
import backend crates directly.

## `client-apple` unsafe carve-out

The crate root uses `#![deny(unsafe_code)]`; the only production unsafe exception
for the Apple networking backend is the focused Objective-C bridge under
`src/backend/apple/`, following the same unsafe-isolated Apple-module precedent
as `kithara-decode`. Unsafe is limited to the files that cross Foundation /
Objective-C boundaries: session setup, delegate class registration, completion
blocks, challenge handling, Objective-C selectors skipped by generated bindings,
and `NSData`/`NSHTTPURLResponse` casts.

The carve-out is intentionally leaf-scoped:

- Safe Rust code calls `AppleNet` / `AppleSession`; it does not import objc2 types.
- `src/backend/apple/**` is listed in
  `.config/ast-grep/rust.no-lint-suppression.yml`; only files that actually use
  unsafe carry `#![allow(unsafe_code)]`.
- New unsafe selectors, raw pointer casts, or unsafe trait impls for the Apple
  client belong in the owning Apple bridge file with local `SAFETY:` comments.

## Decorators

`TimeoutNet<N>` wraps all methods with `tokio::time::timeout` and is exported in the public API. A retry decorator with exponential backoff (retries on 5xx, 429, 408, timeouts; does not retry on other 4xx) is also available, but only via the `NetExt` builder methods — the wrapper type itself is not part of the public surface.

`post_bytes` (POST a body, read the full response body) flows through the same retry and timeout layers as the read methods. It is therefore retried on transient errors like the rest, so it is **at-least-once**: a transient failure after the server already accepted the write can re-send it. Callers issuing non-idempotent requests must carry their own dedup or idempotency key. The caller owns `Content-Type` (and any auth) via the `headers` argument — the layer stays body-agnostic.

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
`post_bytes`, `head`, `get_range`, `stream`):

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
- `ReqwestError` → `NetError` (`From`) — wrap reqwest/wreq transport errors into typed `NetError`; Apple maps `NSError` inside `backend/apple/response.rs`
