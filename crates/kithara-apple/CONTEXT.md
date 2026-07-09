# kithara-apple Context

`kithara-apple` is the canonical owner for Apple C ABI used by Kithara.

## Ownership

- Raw Apple structs, type aliases, constants, and extern declarations live in
  `audio_toolbox::sys`.
- Safe RAII wrappers for `AudioConverter`, `AudioFile`, `AudioBufferList`, and
  POD byte copies live under `audio_toolbox`.
- Accelerate bindings live in `accelerate`.
- Codec decisions, gapless policy, stream read semantics, and resampler
  algorithms must not move into this crate.

## Unsafe Boundary

This crate is the canonical unsafe owner for shared Apple C ABI. Unsafe is
confined to the modules that directly bridge Apple C APIs and is documented at
the call site. Callers should not need local Apple FFI declarations for shared
AudioToolbox or Accelerate operations, and downstream crates should keep their
own unsafe policy strict.

## Accelerate

Accelerate is an implementation support layer, not a resampler backend. The
public helpers expose bounded vector operations such as `copy_f32`, `clear_f32`,
`ramp_f32`, `linear_interpolate_f32`, `quadratic_interpolate_f32`, and
`BiquadFilter`; higher-level crates decide when those operations are useful.
