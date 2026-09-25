# kithara-config-uniffi-probe

Test-only UniFFI values, callbacks, owned handles, and an explicit initialization
model for SDK transport acceptance. The host state here is a test model; product
host and audio-graph acceptance belong to their integration suites.

Run from the workspace with a clean backend checkout at the revision declared in
[CI pins](../../../.config/ci-pins.toml), the pinned Bun and wasm-bindgen tools,
and Chrome for Testing headless-shell:

```sh
just test sdk-web <backend-checkout> <headless-shell>
```

The command generates native metadata and the Wasm adapter, type-checks the SDK,
and runs the main-thread/Worker contract with bounded browser memory and time.
The adapter dependency graph is locked in `wasm.lock`; domain dependency versions
come from the workspace. Generated artifacts stay under `target/config-protocol`.

Production FFI ownership is described in the [crate contract](https://github.com/zvuk/kithara/wiki/kithara-ffi).
