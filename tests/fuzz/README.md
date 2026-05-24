# kithara-fuzz

`cargo-fuzz` harness for Kithara. Each fuzz target lives under `fuzz_targets/` and exercises one parser or state machine with arbitrary byte input.

## Run

Requires nightly toolchain (`cargo-fuzz` uses LibFuzzer, nightly-only).

```sh
cargo +nightly fuzz run <target>
```

For example:

```sh
cargo +nightly fuzz run hls_parsing
cargo +nightly fuzz run stream_read_seek
```

## Targets

- `aes_decrypt` — AES-128-CBC decryption invariants.
- `assets_key` — `AssetStore` key parsing.
- `file_config` — file-source config parsing.
- `hls_parsing` / `hls_internal_parsing` — HLS playlist and tag parsing.
- `play_resource_config` — playback resource configuration.
- `storage_mem_resource` / `storage_mmap_resource` — in-memory and mmap-backed resource stores.
- `stream_read_seek` — `kithara-stream` `Read + Seek` adapter.

## Crashes

When a target crashes, the input is saved in `artifacts/<target>/`. Reproduce with:

```sh
cargo +nightly fuzz run <target> artifacts/<target>/<crash-id>
```

Avoid committing crash artifacts — investigate, fix, then add a regression test in the corresponding crate's `tests/`.
