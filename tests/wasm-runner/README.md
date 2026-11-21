# kithara-wasm-test-runner

The `cargo test` runner for `wasm32-unknown-unknown` test binaries.
`.cargo/config.toml` names it as the target runner, so cargo hands it each
Wasm test binary. It generates the bindings, serves the test page on a
loopback port and runs the tests in a headless browser over `WebDriver`, on
the page (`run_in_browser`) or in a dedicated worker
(`run_in_dedicated_worker`).

```sh
CHROMEDRIVER=/path/to/chromedriver \
  cargo +nightly test --target wasm32-unknown-unknown -p <crate> -- [FILTER] [--exact] [--include-ignored]
```

`CHROMEDRIVER`, `GECKODRIVER` or `SAFARIDRIVER` selects the browser, and
`<VAR>_ARGS` passes extra driver arguments. Session capabilities come from
`webdriver.json`. The media fixtures come from `test_server`, which
`kithara-integration-tests` builds next to the runner; the runner starts it
unless one already answers on `TEST_SERVER_PORT`.

The `lock` test fails when the bindgen generator, the `wasm-bindgen` runtime
and the `wasm-bindgen-test` harness resolve to different versions. The
`protocol` test runs the `kithara-wasm-runner-fixture` binaries in both modes
and is selected only by the browser lanes.
