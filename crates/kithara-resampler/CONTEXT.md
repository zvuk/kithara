# kithara-resampler - Context

Detailed contracts and invariants for the kithara-resampler crate; the README
is the overview.

## Ownership

`kithara-resampler` owns standalone sample-rate resampling:

- `Resampler` defines the standalone PCM processing contract.
- `ResamplerBackend` is the trait implemented by built-in and external backend
  factories. `ResamplerConfig<B>` carries a backend factory value directly.
- `ResamplerCapabilities` reports fixed-ratio, variable-ratio, glide,
  standalone, latency-reporting, and real-time properties.
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
`ResamplerCapabilities`. Fixed-ratio analysis config never asks for DJ
pitch/glide modes.

`ResamplerQuality` adjusts implementation quality inside a selected backend. It
does not select a backend. FFT is a built-in backend module selected by config
when that module is compiled, not a quality level. External backends can ignore
quality or map it into their own config.

## Planned Built-In Backend Families

- `Rubato`: fixed-ratio sinc/poly backend, moved from the old decode-owned
  implementation.
- `RubatoFft`: cfg-gated FFT backend variant for explicit offline callers.
- `AppleAudioConverter`: standalone PCM-to-PCM `AudioConverter` backend on
  macOS/iOS. Codec-embedded Apple decode remains in `kithara-decode`.
- `ReadHead`: Rust port of the moving read-head renderer with fixed-ratio
  support first and variable-ratio/glide controls as first-class capabilities.

No Android placeholder exists until there is a real Android PCM resampler
backend.

## Decoder Integration

`kithara-decode` imports this crate and plans one of two placements:

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
<tr><td><code>resample-rubato</code></td><td>no</td><td>Rubato fixed-ratio backend family</td></tr>
<tr><td><code>resample-fft</code></td><td>no</td><td>Rubato FFT backend variant for explicit callers</td></tr>
<tr><td><code>apple</code></td><td>no</td><td>Apple <code>AudioConverter</code> PCM resampler on macOS/iOS</td></tr>
<tr><td><code>resample-readhead</code></td><td>no</td><td>Moving read-head backend</td></tr>
</table>

## Module Layout

- `src/backend.rs` - backend factory trait.
- `src/capabilities.rs` - `bitflags` capability set.
- `src/config.rs` - `ResamplerConfig`, `ResamplerOptions`, quality, and glide
  config.
- `src/error.rs` - construction and processing errors.
- `src/factory.rs` - standalone backend construction.
- `src/mode.rs` - fixed-ratio and variable-ratio mode types.
- `src/placement.rs` - placement vocabulary shared with decode planning.
- `src/traits.rs` - public processing traits and status types.
