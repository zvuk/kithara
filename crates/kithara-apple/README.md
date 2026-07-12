# kithara-apple

Apple platform ABI and safe wrappers shared by Kithara crates.

This crate owns raw AudioToolbox, Accelerate, and Foundation binding surfaces.
Higher-level crates use its typed wrappers or re-exported Apple framework types
instead of declaring local Apple FFI structs, externs, or binding dependencies.
Codec policy remains in `kithara-decode`; resampler algorithms remain in
`kithara-resampler`; HTTP semantics remain in `kithara-net`.
