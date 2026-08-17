# kithara-stretch - Context

Contracts and invariants for the kithara-stretch crate; the README is the overview.

## Ownership

This crate owns pure time-stretch DSP only. Audio-graph glue (`StretchControls`,
`TimeStretchProcessor`, `PcmChunk`, `PcmMeta`, resampler-rate routing) stays in `kithara-audio`,
which passes its existing `PcmPool` through `StretchOptions`; `kithara-stretch` must not create a
default or global pool.

- `StretchBackend` / `StretchBackendError` define the backend contract.
- `StretchKind` is the compiled backend selector. Persisted discriminants are stable regardless of
  which variants are compiled in: `1 = Signalsmith`, `2 = Bungee`; `3` is reserved for a future
  pure-Rust native backend. An unknown discriminant decodes to `StretchKind::all()[0]`, the first
  compiled-in backend, which is also `Default`.
- `StretchOptions` (a `#[non_exhaustive]` `bon` builder) owns backend construction settings: source
  sample rate, channel count, `max_input_frames` (default 8192), and the injected `PcmPool`.
- `build_backend(kind, &options)` dispatches selector → concrete backend.
- `ElasticEngine` / `ElasticPriming` define the exact-span contract, and `ElasticCapabilities` is
  how an engine declares its shape, latency and rate window. Nothing above the adapter names a
  library: the planner, the request and the conformance suite read capabilities only.

## Exact-span contract

`ElasticEngine` renders exactly `request.output_frames()` from exactly `request.source_frames()`;
the frame counts are the only rate control, so the caller owns the transport and two engines fed the
same plan advance through the source identically. `prepare` allocates outside the render core;
`capabilities()` is fixed for the engine's lifetime; `reset()` clears history and may fail for an
engine that clears state by rebuilding itself.

Each engine declares its own `ElasticRateEnvelope` (source advance per output frame) and its own
`ElasticLatency`. Envelope comparisons admit one floating-point rounding step at a declared
boundary but reject the next representable value, so coordinate interpolation cannot turn an exact
contract edge into a rate violation. Request shape, buffer lengths and the rate window are checked
once, by `ElasticCapabilities`, so every engine accepts and rejects the same requests.

`ElasticPriming` is separate because it is not universal. Priming resets the engine, absorbs the
declared source history **without emitting it**, and discards exactly the declared output latency,
so the next `process` starts at the source frame after the warmup span with no leading gap. An
engine whose pipeline can only emit what it has already consumed cannot do this and does not
implement the trait.

## Backend contract

Backends process interleaved `f32` PCM. `set_ratio` and `set_pitch` are independent controls — that
decoupling is what makes keylock real. `set_ratio(stretch)` is the time factor
`output_frames / input_frames` (above `1.0` lengthens the output); `set_pitch(scale)` is the pitch
factor (`1.0` keeps pitch locked). Both reject non-finite or non-positive values with
`StretchBackendError::Param`. `Process` errors propagate through the resident tempo stage as typed
decode failures; source PCM is never silently dropped and the backend never panics across the
adapter boundary.

The produce path must stay allocation-free in steady state. Before playback the resident tempo
stage reserves both `max_output_samples(input_frames)` and `max_tail_samples()` and then reuses the
same scratch across source quanta and drains. A backend must never append beyond either declared
bound. Backends that need planar scratch use the `PcmPool` supplied in `StretchOptions`; no backend
owns a global pool.

`flush(out)` drains the buffered tail at end of stream or at a real region ratio boundary. It is a
one-shot tail drain: repeated flushes without new input or `reset` append nothing, so an EOF drain
can advance under bounded fixed-size output credits until it yields an empty append. A backend that
cannot expose a true tail drain must document that in its adapter. `reset()` clears buffered state
after seek, source-spec change, backend swap, or return to unity passthrough; a spec change is
handled by the caller rebuilding the backend with the new scalar sample rate and channel count, so
the trait intentionally does not depend on `kithara-decode::PcmSpec`.

## Backend limitations

- Bungee has no tail drain (its high-level `Stream` exposes none, and feeding muted input would emit
  stretched silence instead of the buffered tail, inflating duration): `flush` is a no-op and roughly
  one latency of audio is dropped at end of stream. A real drain needs the low-level granular
  `Stretcher` API.
- Bungee construction fails with a typed error when the pool budget cannot cover its planar scratch
  or `Stream::new` fails; an unusable backend is never installed as a silent disabled instance.
- `BungeeElastic` does not implement `ElasticPriming`, for the same root cause as the missing tail
  drain. Its `Stream` emits with a fixed lag: the input-frame coordinate of the next output frame is
  always `emitted_output_frames - latency`, so absorbing history costs exactly as much emitted
  output as it consumes input, and no history/warmup pair leaves the engine aligned. A primed
  Bungee engine needs the low-level granular `Stretcher` API, whose `Request::position` is a source
  coordinate.
- `BungeeElastic::reset` rebuilds the stream (the high-level `Stream` has no reset), so it
  allocates and can fail; `SignalsmithElastic::reset` is allocation-free. Callers that reset on the
  audio thread must account for that.
- Bungee reports its latency only once a grain has been analysed, and the value keeps growing until
  the pipeline is full; it also moves with the rate. `BungeeElastic` therefore saturates the number
  on a throwaway stream at prepare and reports that unity steady-state value for its lifetime.
- `BungeeElastic` declares a `0.5..=2.0` source-advance window and `SignalsmithElastic` declares
  `2/3..=4/3`. Both are the windows the conformance suite verifies, not library limits; widen a
  window together with the tests that exercise its edges.
- Bungee on iOS is opt-in. Its CMake C++ build must see `IPHONEOS_DEPLOYMENT_TARGET`; `xtask apple`
  exports the value from `[workspace.metadata.apple] deployment-target` before invoking
  `cargo swift package`. Preserve the same env for manual `-F stretch-bungee` Apple builds.

## Adding a backend

1. Add `src/backends/<name>.rs` with a concrete adapter implementing `StretchBackend`, re-exported
  from `backends/mod.rs` under the same gate.
1. Add a feature `stretch-<name>` in `Cargo.toml` and to the `any(...)` guard of the
  `compile_error!` in `lib.rs` (the crate requires ≥1 backend).
1. Gate the adapter module, the `StretchKind` variant, its `all()` entry, its `From`/`u8` arms, and
  the `build_backend` factory arm on `#[cfg(feature = "stretch-<name>")]`; keep the discriminant
  stable.
1. Add a `<Name>Elastic` engine in the same file when the library can render exact spans: declare
  its rate window and latency, implement `ElasticEngine`, implement `ElasticPriming` only if it can
  absorb history without emitting it, and add one `elastic_engine_conformance!` line (plus
  `elastic_priming_conformance!` when it primes) in `tests/elastic.rs`. The suite is the contract;
  the only backend-specific test is the one asserting the declared window and latency.
1. Document any target, tail-drain or priming limitation above.

Do not declare `stretch-native` or add `backends/native.rs` until the pure-Rust engine exists.

## No-backend and wasm builds

There is no "no backend" build here: `lib.rs` `compile_error!`s unless at least one `stretch-*`
feature is set, and the machinery (kind, factory, options, backends) is unconditional. "Stretch is
absent" lives one level up — `kithara-audio` depends on `kithara-stretch` **optionally** (only its
`stretch-signalsmith` / `stretch-bungee` features pull it), so a build with no stretch, including
every wasm build today, simply does not link this crate. Domain types that non-stretch code needs
(`GridSegment`, `RegionPlan`) therefore live in `kithara-audio`. The C++ backends are native-only
(`wasm32-unknown-unknown` has no libc++), and `kithara-bufpool` is likewise an optional non-wasm
dependency pulled in by the backend features.
