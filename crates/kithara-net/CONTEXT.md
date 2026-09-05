# kithara-net — Context

Contracts and invariants for `kithara-net`; the README is the overview.

## HTTP client backends

Cargo features select the transport. The choice is invisible above the `Net` trait — `HttpClient`, `NetOptions`, and
`NetError` are backend-agnostic.

| Feature | Client | TLS | Targets | Notes |
|---|---|---|---|---|
| `client-reqwest` (**default**) | `reqwest` | `tls-rustls` (default) / `tls-native` | native + wasm | Pure-Rust, portable. The only backend on wasm32. |
| `client-wreq` | `wreq` | `BoringSSL` (fixed) | native only | Browser TLS/HTTP2 emulation (`ImpersonatePreset`) to defeat anti-bot WAF JA3 fingerprinting. |
| `client-apple` | `NSURLSession` | Apple platform trust | macOS + iOS only | Foundation bindings come from `kithara-apple/foundation`. |

- **At least one usable backend.** `backend/mod.rs` raises `compile_error!` when no backend feature is selected, or when
  wasm32 is built without `client-reqwest`. There is no silent default-to-reqwest.
- **Features are additive; selection is cfg-priority, not mutual exclusion.** Native `client-wreq` shadows
  `client-reqwest`; on macOS/iOS `client-apple` shadows the whole reqwest/wreq seam (`src/client.rs` is not even compiled).
- **The TLS axis applies only to `client-reqwest`.** No-op under `client-wreq` (always BoringSSL), `client-apple`
  (platform trust), and on wasm (browser owns TLS, so `wreq` is gated out of wasm builds entirely).
- Reqwest is the default because feature unification makes "disable a transitive default" impossible in practice while
  "add a forwarded feature" composes, so the backend pulling a C toolchain (`wreq` → BoringSSL) must be opt-in.
- Minor features: `mock` exposes `crate::mock::NetMock` (native only); `perf` attaches `hotpath::measure` to the `RawHttp`
  `Net` methods; `probe` is a pass-through flag with no code in this crate.

## Backend seam

`client.rs` owns the shared `RawHttp` / `HttpClient` logic against the uniform `backend` seam and contains no backend
selection. When `client-apple` is not active, the seam re-exports `Client`, `RequestBuilder`, `Response`, `StatusCode`,
`BackendError`, `build_client(&NetOptions, &ConnectionMetrics)`, `head_request(&Client, &Url)`, and
`post_request(&Client, &Url, Bytes)`. `ClientBuilder`, `apply_compression`, and the connector-metrics layer stay under
`backend/native/`; shared HTTP code must not import backend crates directly. The Apple adapter (`backend/apple/`) bypasses
that seam — Foundation streams through a delegate, not a Rust response type — and exports only `HttpClient = AppleNet`.
On wasm, `head` is a one-byte ranged `GET`.

No native client sets a client-level `read_timeout` or wall-clock timer: the idle timer is owned by `resumable_body`,
whose `sleep` routes through `kithara_platform::time` and collapses under `flash`. A second wall-clock timer would
double-own the stall and break simulation determinism.

## Content-coding contract

`NetOptions::compression` owns content-coding negotiation; caller-supplied `Accept-Encoding` headers are dropped, never
merged. Whole-body `get_bytes` / `post_bytes` advertise exactly the configured set (`AcceptEncodingPolicy::Configured`).
`stream`, `get_range`, and `head` advertise `identity`, because byte offsets must stay in the same representation as
cached bytes and `Content-Length`. A 2xx response still carrying a non-identity `Content-Encoding` under the identity
policy is rejected as `NetError::Decode` before any body byte reaches a downloader or asset store.

The request-level header is authoritative even under wreq emulation: disabling a decoder with `ClientBuilder::no_*` is not
enough, because an emulation preset may already have installed its own header. Apple preserves Foundation's auto-decode of
whole-body responses by dropping the now-stale `content-encoding` and encoded `content-length` response headers.

## Range response contract

Every streaming response validates its status and range headers before exposing the body. A `206 Partial Content`
response must carry a parseable `content-range` whose start matches the request, whose inclusive end does not exceed the
requested bound, and whose numeric total exceeds that end. Unknown `*` totals are rejected because neither the network
stream nor its caller can prove representation completion. A partial response must also carry a parseable
`content-length` exactly matching the declared range span, so a chunked or overlong body cannot escape its interval. A `200 OK` may
represent a server that ignored `Range`, but it must not carry `content-range`; other successful statuses are invalid for
a range request. Any mismatch is a fatal `NetError::Decode` with the request URL and range, so a resumed body cannot append
bytes from a different interval or commit a truncated asset. Streaming calls that did not request a range reject
unsolicited `206` responses. The same validation owns initial requests and resumable re-fetches on every backend.
The resumable stream pins the first known representation total and rejects a re-fetch whose known total conflicts, even
when the resumed byte interval itself is valid. It also preserves the first response envelope: recovery may fill missing
bytes in that response, but cannot expose bytes beyond its declared length.

## Apple backend

- **No unsafe.** The crate root is `#![forbid(unsafe_code)]`. All Objective-C glue (delegate class, blocks, selectors)
  lives behind the safe `kithara_apple::foundation` facade; `kithara-net` declares no `objc2` / `block2` dependency. New
  Foundation bridging belongs in `kithara-apple`.
- **Sessions are shared process-wide, not per client.** `AppleSession` resolves through a process-global registry keyed by
  `SharedSessionKey { is_insecure, max_connections_per_host }`, so every `HttpClient` with matching options reuses one
  ephemeral `NSURLSession` and one Foundation connection pool. Do not split data and streaming requests onto separate
  sessions without a new explicit pooling contract.
- **Pool knob mapping.** `pool_max_idle_per_host` is applied as Foundation's `HTTPMaximumConnectionsPerHost` (a cap on
  simultaneous persistent connections per host — the closest documented control, not the same semantics) and only when it
  converts to a positive `NSInteger`; otherwise Foundation's default stands. Do not substitute `NSInteger::MAX` or another
  sentinel.
- **Timeout ownership matches the other backends.** Header/data establishment is wrapped in the Rust-side
  `inactivity_timeout`; body inactivity stays with `resumable_body`. No Foundation timeout is configured, so Foundation
  cannot race the flash-aware idle timers.
- **Cancellation.** `wait_for_data` / `wait_for_stream_head` cancel the task on token fire, RAII guards cancel a task
  dropped mid-startup, and `AppleBodyStream` registers a cancel waker so a parked poll wakes promptly. Dropping a body
  stream before EOF cancels the task. Cancellation observed after `content-length` bytes already arrived ends the stream
  as a clean EOF, not `NetError::Cancelled`.
- **Body backpressure.** Delegate callbacks push chunks into `AppleBodyQueue`; at `body_queue_capacity` chunks the task is
  suspended and resumes once the queue drains to `body_queue_resume_at`. Capacity `0` disables suspension.
- **`is_insecure`** is honoured in the delegate's authentication-challenge handler (`UseServerTrustCredential`), not
  through a client builder flag.

`HttpClient::connection_count()` is the opened-connection counter behind shared-pool regression tests: native counts
successful connector-layer opens, Apple counts `NSURLSessionTaskMetrics.transactionMetrics` entries with
`isReusedConnection == false`, wasm always reports zero. Server-side TCP instrumentation belongs in test-server support,
never in production code.

## Layering and retry budgets

`HttpClient` (or `AppleNet`) is `RetryNet<Raw*, DefaultRetryPolicy>` over the raw one-shot client; `Raw*` is never
constructed by callers. `HttpClient::new(options, pools, cancel)` panics if the backend builder fails, and `cancel` MUST come
from the consumer crate's cancel tree (`master_cancel.child()` at `App` / `Queue` / FFI player) — orphan tokens are
forbidden in production code. `with_observer` rebuilds the retry layer around the *same* inner client and
`ConnectionMetrics`, so swapping observers never reopens the pool.

Two independent budgets, each sized by `RetryPolicy::max_retries`:

- `RetryNet` retries whole calls. A transient error surfacing after a non-zero budget was spent is promoted to a terminal
  `NetError::RetryExhausted` (Fatal) so downstream treats it as a give-up, not a retry signal. Under `max_retries == 0`
  the decorator is a deliberate pass-through and the raw transient error propagates unchanged.
- `resumable_body` heals one established body: on a stall (no chunk within `inactivity_timeout`), a transient chunk error,
  or a clean EOF before the promised `content-length`, it re-fetches `bytes=base_start+consumed-end`. A `206` resume
  yields `skip = 0`; a `200` (server ignored `Range`) yields `skip = base_start + consumed`, so the consumer still sees
  one continuous, non-duplicated stream. Fatal causes and cancellation end the stream at once.

Every streaming fetch (`stream`, `get_range`) is wrapped by `resumable_body` before reaching a consumer. `get_bytes` /
`post_bytes` do not ride the resilient stream, so their body collection is separately bounded by `inactivity_timeout` per
chunk.

## Decorators

`TimeoutNet<N>` wraps every method in `kithara_platform::time::timeout` and is public. The retry wrapper is reachable only
through `NetExt::with_retry`; the type itself is not nameable outside the crate.

`post_bytes` flows through the same retry layer as the read methods, so it is **at-least-once**: a transient failure after
the server already accepted the write can re-send it. Callers issuing non-idempotent requests must carry their own
idempotency key. The caller owns `Content-Type` and auth via `headers` — the layer stays body-agnostic.

## Timeouts

`NetOptions::inactivity_timeout` (default 30s) is the only request-level limit, applied to all five methods. It bounds
each read gap — establish (connect + response headers), each body chunk, and the diagnostic error-body read — and never
the total request lifetime, so a slow stream that keeps delivering chunks is not aborted. There is no total-lifetime cap
in `NetOptions`; callers wanting one compose `TimeoutNet`. The 30s default absorbs realistic mobile stalls (TCP
retransmits, captive-portal warm-up, TTFB spikes) — the player contract is "wait for the segment regardless of connection
speed", and a 10s cap raced real fixtures.

## Options, errors, headers

`NetOptions` is a `bon` builder with defaults: `compression` all four codings, `inactivity_timeout` 30s, `impersonate`
`Safari`, `retry_policy` (3 retries / 100ms base / 5s max, exponential ×2), `is_insecure` false, `body_queue_capacity` 32,
`body_queue_resume_at` 16, `pool_max_idle_per_host` 8, and `pool_idle_timeout` 5s. The application-owned
`PoolRegion<S>` is an explicit `HttpClient` constructor dependency and stays out of network policy. `pool_idle_timeout` reaches the two
native client builders only; the Apple backend has no Foundation equivalent for it. Native clients additionally enable
the cookie store.

`HttpClient::with_observer` rebuilds `NetOptions` by struct update, so a new option is carried over by construction —
a field-by-field rebuild would silently reset any knob it forgot.

`NetOptionsPatch` and `RetryPolicyPatch` are what a configuration document may say about those options. They are
`Deserialize` only: a patch reaching this crate has already had its references resolved, so nothing serializes one back
out. Every field is optional and a patch writes only the fields it names, leaving the builder's value standing
everywhere else; `retry_policy` is itself a patch, so a document naming one retry field keeps the other two.
`observer` is not in the patch at all — an owner is wiring, not configuration. Durations are `humantime` strings
(`250ms`, `2s`), enums are snake_case names, and an unknown key is refused by name rather than ignored.

`Compression` reads from a document as the list of `CompressionAlgorithm` names it spells, so the flags stay this
crate's own spelling of the setting; an empty list is `Compression::empty()` — negotiation off.

Retryability is decided from the typed `NetError` discriminant, never by substring matching: `Timeout`, `Network`, and
`Status` with 5xx / 429 / 408 are `Transient`; everything else — including `Decode`, `Cancelled`, and `RetryExhausted` —
is `Fatal`. Error bodies kept in `NetError::Status` are truncated to 200 chars.

`Headers` wraps `HashMap<String, String>` and is **case-sensitive**. Response extraction lowercases header names on both
native (reqwest already normalizes) and Apple paths; lookups of headers this crate did not normalize must try both cases.
`head` backfills a missing `content-length` from the total in `content-range` (ignoring `*`) on both paths, so a
HEAD-hostile server still yields a size.

## Trait bridges

- `RangeSpec: Display` — renders `bytes=start-end` / `bytes=start-`.
- `HashMap<String, String> → Headers` (`From`, via `derive_more`).
- `&NetError → Retryability` (`From`) — the single retry classifier.
- `BackendError → NetError` (`From`) — reqwest/wreq transport errors; non-status errors become retryable `Network`
  carrying the full source chain. Apple maps `NSError` in `backend/apple/response.rs` (`-999` → `Cancelled`, `-1001` →
  `Timeout`).
- `ImpersonatePreset → wreq_util::Profile` (`From`) — `client-wreq` only; `Safari` → `Safari18`, `Chrome` → `Chrome137`.
- Compression is applied by `apply_compression(ClientBuilder, Compression)`, which disables the decoders *not* in the flag
  set — a function rather than a `From` impl, because the request header stays authoritative.
