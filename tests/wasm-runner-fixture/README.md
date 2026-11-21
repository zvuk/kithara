# kithara-wasm-runner-fixture

Minimal `wasm-bindgen-test` binaries for the `kithara-wasm-test-runner`
`protocol` test: `browser` runs on the page and `worker` in a dedicated
worker. Each exports one passing, one failing and one ignored test. The
browser lanes build them with `--no-run` and pass their paths to the
`protocol` test.
