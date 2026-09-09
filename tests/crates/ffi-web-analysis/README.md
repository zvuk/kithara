# Kithara FFI web analysis tests

Browser integration tests for analysis events exposed by `kithara-ffi`.

The package keeps this focused WASM target out of the native workspace test
lane and separate from the heavier FFI stress and Selenium dependencies.
