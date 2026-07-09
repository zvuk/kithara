# kithara-resampler - Context

Detailed contracts and invariants for the kithara-resampler crate; the README
is the overview.

## Ownership

`kithara-resampler` owns the platform-neutral standalone sample-rate
resampling contract:

- `Resampler` defines the standalone PCM processing contract.
- `ResamplerBackend` is the trait implemented by built-in, platform, and
  external backend factories. `ResamplerConfig<B>` carries a backend factory
  value directly.
- `ResamplerCapabilities` reports fixed-ratio, variable-ratio, glide,
  standalone, latency-reporting, and real-time properties.
- Backend choice is encoded by the caller's `B: ResamplerBackend` type.
  Playback, analysis, and decoder configs thread that type through their own
  builders; this crate does not expose a cfg-selected default backend handle.
  Platform backend contracts such as Apple AudioConverter live here and use
  `kithara-apple` for shared Apple ABI and safe AudioToolbox wrappers.
- `ResamplerSettings`, `ResamplerConfig`, and `ResamplerOptions` carry
  construction settings through BON builders.
- `create_resampler` constructs exactly the requested standalone backend or
  returns a typed error.

Decoder placement is not owned here. `kithara-decode` decides whether a decoder
uses a codec-embedded converter or wraps decoded PCM in a decoder adapter. Both
paths use types from this crate, but codec lifecycle, media input, and gapless
translation stay in `kithara-decode`.

Playback graph routing is not owned here. `kithara-audio` passes device-rate
config into decode and analysis, but it must not contain backend-specific
resampler policy.

## Allocation Contract

Hot paths must not allocate through ordinary `Vec` growth. Backends use one of
these storage owners:

- caller-owned input and output slices passed to `Resampler::process_into_buffer`;
- scratch buffers taken from the `PcmPool` inside `ResamplerSettings`;
- backend-owned pooled scratch acquired during construction or reset.

Library code must not call `PcmPool::default()` as a hidden fallback. The pool is
injected by the surface that owns memory sizing, matching the
`kithara-bufpool` contract. Construction may pre-warm scratch for the configured
channels and chunk size. Steady-state processing reuses already-owned buffers.

Any backend adapter that has to bridge a third-party API shape, such as Rubato's
planar adapter surface, must hide that bridge behind pooled scratch and keep the
public trait on borrowed planar slices.

## Backend Contract

The base `Resampler` trait is for standalone PCM-to-PCM processing. It accepts
borrowed planar `f32` slices and writes into caller-owned planar output slices.
The returned `ResamplerProcess` reports accepted input frames and produced
output frames. Callers size output using the backend's frame-capacity methods.

Variable ratio and glide controls are advertised through
`ResamplerCapabilities`. Callers that hold a concrete resampler use
`Resampler::control_mut()` to access the optional `ResamplerControl` surface;
fixed-ratio backends return `None`. Fixed-ratio analysis config never asks for
DJ pitch/glide modes.

`ResamplerQuality` adjusts implementation quality inside a selected backend. It
does not select a backend. Rubato-specific algorithm choices, including FFT, are
`rubato::RubatoConfig` fields, not separate backend families or cargo features.
External backends can ignore quality or map it into their own config.

Once construction receives a backend value, it builds exactly that backend or
returns a typed error. There is no runtime fallback chain and no portable
default backend in this crate.

## Built-In Backend Families

- `Rubato`: fixed-ratio backend moved from the old decode-owned
  implementation. `rubato::RubatoConfig` selects async poly/sinc or FFT.
- `Glide`: Rust port of the cursor-based glide renderer with fixed-ratio
  support, variable-ratio controls, and ratio glide. The scalar path is the
  portable baseline. On macOS/iOS, the optional `apple-accelerate` feature uses
  `kithara-apple::accelerate` for copy, interpolation, and biquad filtering.

## Platform Backend Families

- `AppleAudioConverter`: standalone PCM-to-PCM `AudioConverter` backend on
  macOS/iOS. `apple::AppleAudioConverterBackend` and
  `apple::AppleAudioConverterConfig` live in this crate and accept an
  `AudioConverterFactory`. The current concrete AudioToolbox factory,
  `apple::AudioToolboxConverterFactory`, uses `kithara-apple` wrappers instead
  of declaring local AudioToolbox FFI. This crate root denies unsafe code;
  unavoidable Apple unsafe lives in `kithara-apple`. Codec-embedded Apple decode
  remains in `kithara-decode` but uses the same shared Apple ABI crate.

No Android placeholder exists until there is a real Android PCM resampler
backend.

Future backend families get cargo features only when their implementation
module lands. Empty placeholder features are not part of the public contract.

## Decoder Integration

`kithara-decode` imports this crate and supports one of two placements:

- codec-embedded: the decoder emits target-rate PCM internally, initially Apple
  decoder plus Apple `AudioConverter`;
- decoder-adapter: any decoder emits source-rate PCM and is wrapped by a
  standalone backend from this crate. The standalone backend may be a built-in
  implementation or any external type implementing `ResamplerBackend`.

Explicit invalid pairs return typed configuration errors. The planner must not
try another backend when the requested pair is unsupported.

Gapless frame metadata belongs to the decoder-output domain. Scaling from source
rate to output rate happens once in the decode plan, independent of whether the
resampler is codec-embedded or adapter-based.

## Feature Flags

<table>
<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>
<tr><td><code>resample-rubato</code></td><td>no</td><td>Rubato fixed-ratio backend; algorithm selection lives in <code>RubatoConfig</code></td></tr>
<tr><td><code>resample-glide</code></td><td>no</td><td>Glide backend with fixed-ratio, variable-ratio, and glide capability</td></tr>
<tr><td><code>apple-accelerate</code></td><td>no</td><td>Apple-target Glide acceleration through <code>kithara-apple::accelerate</code>; ignored on non-Apple targets</td></tr>
</table>

## Module Layout

- `src/apple/` - Apple AudioConverter backend contract, config, and wrapper use;
  shared FFI lives in `kithara-apple`.
- `src/backend.rs` - backend factory trait.
- `src/capabilities.rs` - `bitflags` capability set.
- `src/config.rs` - `ResamplerConfig`, `ResamplerOptions`, quality, and glide
  config.
- `src/error.rs` - construction and processing errors.
- `src/factory.rs` - standalone backend construction.
- `src/glide/` - Glide backend plus optional Apple Accelerate engine.
- `src/mono.rs` - pooled mono streaming adapter used by beat analysis.
- `src/mode.rs` - fixed-ratio and variable-ratio mode types.
- `src/rubato/` - Rubato backend.
- `src/traits.rs` - public processing traits and status types.
