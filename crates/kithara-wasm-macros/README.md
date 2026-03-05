# kithara-wasm-macros

## Purpose
Proc-macro crate for wasm-specific ergonomics.

## Owns
- `#[wasm_export]` method-to-free-function export generation
- `#[assert_not_main_thread]` runtime guard injection for worker entrypoints

## Integrates with
- `kithara-wasm`

## Notes
- Keep macros focused on codegen glue only; no runtime policy here.
