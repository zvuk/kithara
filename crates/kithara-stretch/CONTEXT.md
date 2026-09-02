# kithara-stretch - Context

Contracts and invariants for the kithara-stretch crate; the README is the overview.

## Ownership

This crate owns pure time-stretch DSP engines only. `kithara-warp` owns the
synchronous `WarpRenderer`, `StretchControls`, and `RegionPlan`, passing the
play-configured `PoolRegion<S>` through `ElasticConfig<S>`; `kithara-signal` owns
`AudioChunk`, `AudioChunkInfo`, `AudioSpec`, and the canonical sample views.
Decoder sample-rate conversion remains in the decode/audio seam. This crate must
not create a default or global pool.

- `ElasticEngine` is the sole backend contract. Exact source/output frame counts control time;
  `set_pitch` remains independent, and `prime` / `flush` / `reset` define stream lifecycle.
- `StretchKind` is the compiled backend selector. Persisted discriminants are stable regardless of
  which variants are compiled in: `1 = Signalsmith`, `2 = Bungee`; `3` is reserved for a future
  pure-Rust native backend. An unknown discriminant decodes to `StretchKind::all()[0]`, the first
  compiled-in backend, which is also `Default`.
- `ElasticConfig<S>` is the single fallible `#[non_exhaustive]` `bon` root config. It owns the
  `StretchKind` selection, sample rate, channel count, maximum source/output frame spans and the
  practical playback-rate envelope, plus the injected pool region; the selector is not a second
  factory argument.
- `build_engine(config)` dispatches the config-owned selector to `Box<dyn ElasticEngine>`.
- Every backend must implement priming; callers may still render a fresh unprimed stream. Nothing
  above an adapter names a concrete DSP library.

## Exact-span contract

`ElasticEngine` renders exactly `request.output_frames()` from exactly `request.source_frames()`;
the frame counts are the only rate control, so the caller owns the transport and two engines fed the
same plan advance through the source identically. `prepare` allocates outside the render core;
`capabilities()` is fixed for the engine's lifetime; `reset()` clears history and may fail for an
engine that clears state by rebuilding itself.

Each engine reports one common `ElasticRateEnvelope` plus its own `ElasticLatency`. The configured
rate policy defaults to the practical `0.05..=4.0` range and preparation intersects it with ratios
representable by the fixed source/output frame limits. Request shape, buffer lengths, prepared
limits and the rate window are checked once by `ElasticCapabilities`, so every engine accepts and
rejects the same requests before entering backend code.

For a cue at source frame `C`, `prime` consumes four adjacent regions with no hidden coordinate
change: history `[C-H, C)`, lookahead `[C, C+H)`, warm source
`[C+H, C+H+rate*O)`, then the next `process` source. `H` and `O` are the declared source and output
latencies; the warm request therefore contains exactly `rate*O` source frames and `O` output
frames. Priming clears old state and writes its `O` warmup frames into caller-owned discard storage,
but the first subsequent audible output starts at `C`, not after the warm source. A backend that
cannot preserve this ordering does not implement `ElasticEngine`.

## Engine contract

Engines process interleaved `f32` samples. `ElasticRequest` names exact non-empty source and output
frame spans; their ratio is the only tempo control. `set_pitch(scale)` is independent (`1.0` keeps
pitch locked), which preserves keylock without a second streaming API. Invalid preparation,
requests, pitch or processing return `ElasticError`; the outer `WarpRenderer::render` maps failure
to "drop this chunk + warn", never a panic.

The produce path must stay allocation-free. Callers provide fixed output slices from scratch
reserved before the checked render call, and an engine that needs planar scratch checks it out from
the `PoolRegion<S>` supplied in `ElasticConfig<S>`; no engine owns a default or global pool. Bungee keeps
that channel-major scratch in `kithara_signal::PlanarBuffer` instead of a backend-local buffer type.

`flush(out)` writes the next buffered-tail portion into non-empty caller-owned storage containing
only complete interleaved frames and returns its frame count together with whether that portion
completed the drain. The caller normally supplies one render quantum and repeats the call until
completion; a completed drain stays empty until new input.
An active drain reports completion on its final non-empty portion, so the caller can publish the
released source frontier with those samples. This streaming contract lets a
rate-dependent tail span several fixed-size chunks without loss. At a steady rate `r`, the complete
terminal span is `ceil(H / r) + O` frames, where `H` and `O` are the declared source and output
latencies. The same formula applies after a rate change has rendered `O` frames and reached its new
latency mapping. EOF during that bounded transition drains the real accumulated state, whose span is
history-dependent; it must not reset, replay, reverse, drop, or synthesize source audio to imitate the
settled formula. Every backend must expose this real terminal-drain behavior; a no-op or
synthetic-silence flush is not conforming.
A rate change between adjacent exact requests preserves history, moves monotonically, and reaches
the requested mapping within one declared output-latency window. Pitch changes must affect audio
within the same bound. A backend must not hide already-rendered samples behind an additional software
delay.
`reset()` clears buffered state after seek; source-spec and backend changes are handled by the
caller preparing a replacement outside its checked render core. The trait intentionally does not
expose `kithara_signal::AudioSpec`; native adapters use it only to shape canonical sample buffers.

## Backend limitations

- Bungee uses the low-level `bungee-sys` granular `Stretcher` API behind a private RAII owner. Native
  planar output is validated and copied immediately into pooled Rust storage; no native pointer or
  mutable slice escapes the call that produced it.
- Bungee retains the source lookahead and output remainder required by its overlapping grains. EOF
  first advances finite requests to the exact source end, clips output by native request timestamps,
  then clears the four-grain pipeline with invalid requests. `flush` therefore returns real terminal
  audio across one or more chunks instead of dropping roughly one latency of the track.
- Once one exact rate has rendered for a complete output-latency window, Bungee bounds EOF with the
  integer `ElasticRequest` ratio and the common settled-tail formula. Before that point its real
  history-dependent native timestamps remain authoritative. Repeated floating-point native hops
  therefore cannot add a frame to a settled tail or replace a transitional tail with a formula.
- Bungee preparation fails when the injected region budget cannot cover its planar scratch or
  native stretcher construction fails; the audio adapter warns once and marks the engine unavailable.
- Bungee priming drains old resident state, stages history, lookahead and warm source in the
  injected pooled rolling source buffer, and performs native preroll without rebuilding the
  stretcher. Preroll discards only native output ending at or before the cue and stops before
  scheduling the cue grain. The next `process` applies its current rate and pitch when it schedules
  that grain; only the unconsumed remainder of one native output chunk may cross a render boundary.
  There is no software post-cue sample queue, and the render path allocates nothing.
- The private Bungee adapter clears and drains the resident native pipeline on `reset` without
  rebuilding it; its Rust-side input/output storage remains the buffers reserved from the injected
  pool at prepare.
- Bungee reports its unity latency only after its pipeline is warm, and runtime latency moves with
  the rate. Preparation measures the unity reference on the resident, shape-sized core and resets
  that same core in place; no probe engine or extra pool allocation is retained. An unprimed stream converts
  the larger native processing center into the measured source/output latency split while its cold
  pipeline fills. Once output starts, rate changes slew that center toward the new mapping with grain
  positions clamped to the configured rate envelope. Positions therefore remain strictly monotone,
  phase history survives without a native reset, and the settled terminal timing reaches the final
  request's `ceil(H / r) + O` split within `O` output frames.
- Both engines accept the same pitch range, `0.25..=4.0`; this is the range covered by Bungee's
  native sizing and prevents a backend selector from changing validation semantics.
- Both engines expose the configured practical rate policy after intersection with their prepared
  source/output frame limits; the conformance suite exercises its minimum, unity and maximum
  requests and rejects extreme in-shape ratios before backend access.
- Bungee on iOS is opt-in. Its CMake C++ build must see `IPHONEOS_DEPLOYMENT_TARGET`; `xtask apple`
  exports the value from `[workspace.metadata.apple] deployment-target` before invoking
  `cargo swift package`. Preserve the same env for manual `-F stretch-bungee` Apple builds.

## Adding a backend

1. Add `src/backends/<name>.rs` with a concrete adapter implementing `ElasticEngine`; expose it only
  to the crate-owned factory through a `pub(crate)` re-export in `backends/mod.rs` under the same
  gate.
1. Add a feature `stretch-<name>` in `Cargo.toml` and to the `any(...)` guard of the
  `compile_error!` in `lib.rs` (the crate requires ≥1 backend).
1. Gate the adapter module, the `StretchKind` variant, its `all()` entry, its `From`/`u8` arms, and
  the `build_engine` factory arm on `#[cfg(feature = "stretch-<name>")]`; keep the discriminant
  stable.
1. Declare latency, implement the complete `ElasticEngine` lifecycle, and add the backend as a named
  case in the shared facade and priming matrices in `tests/elastic.rs`. The suite is the contract;
  backend-specific tests cover only preparation mechanics and measured latency.
1. Document any target, tail-drain or priming limitation above.

Do not declare `stretch-native` or add `backends/native.rs` until the pure-Rust engine exists.

## No-backend and wasm builds

There is no "no backend" build here: `lib.rs` `compile_error!`s unless at least one `stretch-*`
feature is set, and the machinery (kind, factory, config, backends) is unconditional. Native
`kithara-play` selects and forwards its `stretch-signalsmith` / `stretch-bungee` backend feature
through `kithara-warp`; its default selects Signalsmith. `kithara-warp` has no default backend and
only compiles `WarpRenderer` when a native backend is selected. A native
facade that disables default features must therefore forward one backend when it enables playback
stretching. Wasm excludes the DSP dependency at the target edge because the C++ backends cannot
build for `wasm32-unknown-unknown`; backend-independent `GridSegment`, `RegionPlan`, and controls
remain in `kithara-warp`.
