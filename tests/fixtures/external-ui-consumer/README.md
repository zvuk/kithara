# External UI consumer probe

This is a separate Cargo workspace. Run Cargo from outside the Kithara checkout
with `--manifest-path` pointing here so the checkout's Cargo configuration and
feature unification cannot supply undeclared requirements.

Set `EXTERNAL_UI_FONT` to an absolute caller-owned Roboto-Regular.ttf path.
The font is deliberately not embedded in Kithara. The LSQ input used for this
probe has SHA256 `dbd285b518e398832f6f4a736109c355ce25a49546bfce41bab256c9ef7e56eb`.

```sh
cargo run --manifest-path "$KITHARA_REPO/tests/fixtures/external-ui-consumer/Cargo.toml" --no-default-features --features render --bin consumer
cargo run --manifest-path "$KITHARA_REPO/tests/fixtures/external-ui-consumer/Cargo.toml" --no-default-features --features vello --bin consumer
cargo run --manifest-path "$KITHARA_REPO/tests/fixtures/external-ui-consumer/Cargo.toml" --no-default-features --features masonry --bin consumer
cargo run --manifest-path "$KITHARA_REPO/tests/fixtures/external-ui-consumer/Cargo.toml" --no-default-features --features masonry --bin host
cargo run --manifest-path "$KITHARA_REPO/tests/fixtures/external-ui-consumer/Cargo.toml" --bin closure
```

`consumer` checks public gradient refusal, typed invalid-font admission, source
lifetime, glyph identity and SVG outlines. With `vello`, it checks the production
backend's encoded font bytes and exact glyph positions. `host` runs that supplied
font through the public retained Masonry route with typed actions, cancellation,
transformed hits and idle/invalidation/finite-animation scheduling.

`closure` asks Cargo for the native Apple Masonry normal/build graph, including
proc-macro host dependencies, and rejects the unrelated workspace audio union.
Its nested Cargo invocation also runs outside the checkout. SVG and graph output
go to `target/acceptance`; preserve the independent lockfile with receipts.

For wasm, use `cargo check --bins --target wasm32-unknown-unknown` with the same
manifest and one feature selection. Compilation does not establish browser or
window runtime support. This probe does not port or replace any LSQ host.

Probe results on 2026-11-13:

| Selection | Native Apple | wasm32 |
| --- | --- | --- |
| render | 3 checks pass | compile-only pass |
| vello | 4 checks pass | compile-only pass |
| masonry | 7 checks pass | unavailable |

Native checks cover the headless backend and retained route. Masonry's wasm
build fails in `masonry_core 0.4.0` because its tracing backend references
undeclared `tracing_wasm`, `console_error_panic_hook` and `Registry`. Browser
and real-window runtime acceptance remain separate.
