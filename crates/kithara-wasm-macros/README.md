<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-wasm-macros

Workspace proc-macro crate (`publish = false`) for wasm export glue and thread-affinity assertions.

## Macros

- `#[wasm_export]` on `impl Player` blocks
- `#[assert_not_main_thread]` on worker-only functions

## `#[wasm_export]`

Generates `#[wasm_bindgen] pub fn player_<method>(...)` free functions for methods marked with `#[export]` inside the impl block.

## `#[assert_not_main_thread]`

Injects `kithara_platform::thread::assert_not_main_thread(...)` at function entry, useful for wasm worker entrypoints.

## Example

```rust
#[wasm_export]
impl Player {
    #[export]
    fn play(&self) {
        // ...
    }
}

#[assert_not_main_thread]
fn worker_main() {
    // worker-only body
}
```

## Integration

Used by `kithara-wasm` to keep wasm-bindgen exports and worker thread guards declarative and consistent.
