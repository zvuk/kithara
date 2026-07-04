# kithara-stretch - Context

Detailed contracts and invariants for the kithara-stretch crate; the README is
the overview.

## Ownership

`kithara-stretch` owns pure time-stretch DSP concerns:

- `StretchBackend` and `StretchBackendError` define the backend contract.
- `StretchBackendKind` stores the compiled backend selector. The persisted
  discriminants are stable: `1 = Signalsmith`, `2 = Bungee`; discriminant `3`
  is reserved for the future pure-Rust native backend.
- `build_backend` dispatches from selector to concrete backend using only
  `sample_rate` and `channels`.
- `RegionPlan`, `GridSegment`, and `ActiveRegion` describe beat-aligned stretch
  regions in source-frame coordinates.

Audio graph glue stays out of this crate. `StretchControls`,
`TimeStretchProcessor`, `PcmChunk`, `PcmMeta`, `PcmPool`, and resampler-rate
routing remain in `kithara-audio`.

## Backend Contract

Backends process interleaved `f32` PCM. `set_ratio` and `set_pitch` are
independent controls:

- `set_ratio(stretch)` is the time factor, `output_frames / input_frames`;
  values above `1.0` make the output longer.
- `set_pitch(scale)` is the pitch factor; `1.0` keeps pitch locked.

The produce path must stay allocation-free in steady state. Callers ask
`max_output_samples(input_frames)` before `process` or `flush`, reserve that
much scratch capacity, and then reuse the same output buffer across chunks.

`flush(out)` drains the buffered tail at end of stream or at a real region
ratio boundary. It is a one-shot tail drain for a processed stream: repeated
flushes without new input or `reset` should append nothing. A backend that
cannot expose a true tail drain must document that behavior in its adapter.

`reset()` clears buffered state after seek, source-spec change, or backend
swap. A spec change is handled by the caller rebuilding the backend with the
new scalar sample rate and channel count; the backend trait intentionally does
not depend on `kithara-decode::PcmSpec`.

## Adding A Backend

To add a backend:

1. Add `src/backends/<name>.rs` with a concrete adapter implementing
   `StretchBackend`.
2. Add a feature for that backend in `Cargo.toml`.
3. Add a `StretchBackendKind` variant and keep its `to_u8` / `from_u8`
   discriminant stable.
4. Re-export the adapter from `backends/mod.rs`.
5. Add a `build_backend` factory arm.
6. Document any target or tail-drain limitations here.

The next reserved discriminant is `3` for the future pure-Rust native engine.
Do not declare the feature or add `backends/native.rs` until that engine exists.

## Wasm

The selector and dispatch modules are gated by backend capability, not by a
hard target exclusion. The current C++ adapters are gated to native targets
because `wasm32-unknown-unknown` has no libc++ environment for them.

The wasm-capable backend is planned as a pure-Rust `stretch-native` engine built
on `rustfft`. It should plug into the same feature, selector, and factory shape
without changing `kithara-audio`.
