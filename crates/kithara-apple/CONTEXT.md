# kithara-apple Context

`kithara-apple` is the canonical owner for Apple framework ABI used by Kithara.

## Ownership

- Raw Apple structs, type aliases, constants, and extern declarations live in
  `audio_toolbox::sys`.
- Safe RAII wrappers for `AudioConverter`, `AudioFile`, `AudioBufferList`, and
  POD byte copies live under `audio_toolbox`.
- Accelerate bindings live in `accelerate`.
- Foundation and Objective-C binding crates are exposed through `foundation`.
  Consumers should depend on `kithara-apple` features rather than declaring
  `block2`, `objc2`, or `objc2-foundation` directly.
- Codec decisions, gapless policy, HTTP semantics, stream read semantics, and
  resampler algorithms must not move into this crate.

## Feature Surfaces

- `audio-toolbox` exposes `audio_toolbox`.
- `accelerate` exposes `accelerate`.
- `foundation` exposes the Objective-C/Foundation binding surface needed by
  Apple platform adapters such as the `NSURLSession` HTTP backend.

## Unsafe Boundary

This crate is the canonical unsafe owner for shared Apple framework ABI. Unsafe is
confined to the modules that directly bridge Apple C APIs and is documented at
the call site. Callers should not need local Apple FFI declarations for shared
AudioToolbox, Accelerate, or Foundation binding dependencies, and downstream
crates should keep their own unsafe policy strict except for leaf adapter glue
that must implement Apple callback protocols.

## Accelerate

Accelerate is an implementation support layer, not a resampler backend. The
public helpers expose bounded vector operations such as `copy_f32`, `clear_f32`,
`ramp_f32`, `linear_interpolate_f32`, `quadratic_interpolate_f32`, and
`BiquadFilter`; higher-level crates decide when those operations are useful.
