# kithara-apple

Apple platform ABI and safe wrappers shared by Kithara crates.

This crate owns raw AudioToolbox and Accelerate bindings. Higher-level crates
use its typed wrappers instead of declaring local Apple FFI structs or externs.
Codec policy remains in `kithara-decode`; resampler algorithms remain in
`kithara-resampler`.
