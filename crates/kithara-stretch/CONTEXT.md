# kithara-stretch - Context

Detailed contracts and invariants for the kithara-stretch crate; the README is
the overview.

## Ownership

`kithara-stretch` owns pure time-stretch DSP concerns:

- `StretchBackend` and `StretchBackendError` define the backend contract.
- `StretchKind` stores the compiled backend selector. The persisted
  discriminants are stable: `1 = Signalsmith`, `2 = Bungee`; discriminant `3`
  is reserved for the future pure-Rust native backend.
- `StretchOptions` owns backend construction settings: source sample rate,
  channel count, max backend input block size, and the injected `PcmPool`.
- `build_backend` dispatches from selector to concrete backend using
  `StretchOptions`.

Audio graph glue stays out of this crate. `StretchControls`,
`TimeStretchProcessor`, `PcmChunk`, `PcmMeta`, and resampler-rate routing remain
in `kithara-audio`. The audio graph passes its existing `PcmPool` through
`StretchOptions`; `kithara-stretch` must not create a default or global pool.

`kithara-stretch` depends downward on `kithara-bufpool` for backend scratch
storage and on `kithara-workspace-hack` for native workspace unification.

The crate also owns the transport-facing numeric elastic DSP contract. The
existing `SignalsmithBackend` family processes both streaming chunks and exact
source/output spans. `ElasticConfig` is the type parameter that selects the
validated exact-span mode; the private streaming mode implements
`StretchBackend`. `ElasticCapabilities` declares the supported rate envelope,
algorithmic latency, channel/sample-rate identity, prepared block limits, and
whether caller-ordered reverse input is supported.
`ElasticRequest` contains only the integer frame counts consumed by the backend.
`ElasticSpan` describes a transport-neutral continuous source path and exact
output length. `ElasticSpanPlan` validates at most four adjacent spans against
one capability snapshot, quantizes them without leaving inline storage, and
returns both backend requests and the staged `ElasticCursor`. It also owns the
bounded inter-block phase correction because that correction is constrained by
the backend rate envelope and exact source/output frame arithmetic.
`ElasticSpanConfig` carries the continuity tolerance, maximum accepted phase
error, and per-block correction budget; these values are supplied by the
reader's configuration rather than hidden in the planner.

Session beats, track bindings, request/revision identity, relocation commit
boundaries, and graph scheduling remain in `kithara-play`. Source-window policy,
bounded PCM polling, DSP readiness, and the numeric cursor belong to
`kithara-audio::elastic::ElasticReader`. A `PhaseDiscontinuity` is numeric
evidence that continuous correction is impossible; the player may map that
evidence to a session relocation, while the audio reader owns its prepared PCM
and cursor. The cursor returned by `ElasticSpanPlan` is staged and must not be
committed until every backend request in the plan succeeds.

The exact-span path currently has one adapter, so callers use the concrete
`SignalsmithBackend<ElasticConfig>` instead of a single-implementation trait
object. Both modes own the same concrete engine family; there is no second
Signalsmith wrapper or runtime unsupported branch. Backend capability selection
remains above this implementation; introduce an interface at this seam only
when a second exact-span adapter exists.

## Backend Contract

Backends process interleaved `f32` PCM. `set_ratio` and `set_pitch` are
independent controls:

- `set_ratio(stretch)` is the time factor, `output_frames / input_frames`;
  values above `1.0` make the output longer.
- `set_pitch(scale)` is the pitch factor; `1.0` keeps pitch locked.

The produce path must stay allocation-free in steady state. Callers ask
`max_output_samples(input_frames)` before `process` or `flush`, reserve that
much scratch capacity, and then reuse the same output buffer across chunks.
Backends that need planar scratch use the `PcmPool` supplied in
`StretchOptions`; no backend owns a global pool.

`flush(out)` drains the buffered tail at end of stream or at a real region
ratio boundary. It is a one-shot tail drain for a processed stream: repeated
flushes without new input or `reset` should append nothing. A backend that
cannot expose a true tail drain must document that behavior in its adapter.

`reset()` clears buffered state after seek, source-spec change, or backend
swap. A spec change is handled by the caller rebuilding the backend with the
new scalar sample rate and channel count; the backend trait intentionally does
not depend on `kithara-decode::PcmSpec`.

`SignalsmithBackend<ElasticConfig>` is prepared for fixed maximum source and
output blocks.
Every numeric request is checked against those limits and the declared rate
envelope before processing. `prime` resets the algorithm, consumes the required
warm-up span, and discards the reported output latency so the caller can align
the first audible frame to its presentation boundary. The backend never chooses
a transport rate, direction, or phase on its own. It declares reverse support
because the caller may supply history, warmup, and steady source spans already
ordered in reverse audible time without changing the numeric request shape.
Envelope comparisons admit one
floating-point rounding step at a declared boundary, but reject the next
representable value; coordinate interpolation therefore cannot reject an exact
2/3 or 4/3 contract edge as a real rate violation.

## Adding A Backend

To add a backend:

1. Add `src/backends/<name>.rs` with a concrete adapter implementing
   `StretchBackend`.
2. Add a feature `stretch-<name>` in `Cargo.toml`, and add it to the `any(...)`
   guard of the `compile_error!` in `lib.rs` (the crate requires ≥1 backend).
3. Gate the adapter module, the `StretchKind` variant, its `all()` entry,
   its `From`/`u8` arms, and the `build_backend` factory arm on
   `#[cfg(feature = "stretch-<name>")]`; keep the variant's discriminant stable.
4. Re-export the adapter from `backends/mod.rs` under the same gate.
5. Document any target or tail-drain limitations here.

Bungee on iOS is an opt-in backend. Its CMake C++ build must see
`IPHONEOS_DEPLOYMENT_TARGET=16.0`; `xtask apple` exports that env before
invoking `cargo swift package`. Preserve the same env for manual
`-F stretch-bungee` Apple builds.

The next reserved discriminant is `3` for the future pure-Rust native engine.
Do not declare the feature or add `backends/native.rs` until that engine exists.

## No-backend and wasm builds

This crate has no "no backend" build: `lib.rs` `compile_error!`s unless at least
one `stretch-*` feature is set, and the whole machinery (kind, factory, config,
backends) is unconditional. The "stretch is absent" case lives one level up —
`kithara-audio` depends on `kithara-stretch` **optionally** (only its
`stretch-signalsmith` / `stretch-bungee` features pull it), so a build with no
stretch — including every wasm build today — simply does not link this crate.
Domain types that non-stretch code needs (`GridSegment`, `RegionPlan`) therefore
live in `kithara-audio`, not here.

The C++ backends are native-only (`wasm32-unknown-unknown` has no libc++);
enabling one on wasm would fail at the C++ compile. The planned pure-Rust
`stretch-native` engine (on `rustfft`) would be wasm-capable, letting a wasm
build opt into stretch by depending on this crate with that feature.
