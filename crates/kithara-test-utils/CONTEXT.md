# kithara-test-utils — Context

Detailed contracts and invariants for the kithara-test-utils crate; the README is the overview.

## Probe capture

The probe capture helper records every `tracing::event!` whose target ends with `_probe` (all `#[kithara::probe]` expansions emit to `<crate_name>_probe`, e.g. `kithara_stream_probe`). Events are kept in a process-wide `Vec` so a test can snapshot the full sequence and assert on it.

- **Why a tracing layer (not just `EventBus`)**: `kithara_events::EventBus` is a `tokio::sync::broadcast` — under load, lagged subscribers drop events. Probes fire at the decision site and the tracing layer records every emission without a bounded channel (defense in depth).
- **Why a process-wide subscriber**: `tracing::subscriber::set_default` is thread-local, but probes fire on the tokio worker threads spawned by `Downloader::run`, which don't inherit a per-test default. A single global subscriber is installed and captured events are filtered per-test by start timestamp. Because `#[kithara::test]` initialises a global subscriber via `setup_tracing_with_filter`, the probe layer must be attached inside that init path (`fixtures::init_tracing` installs it alongside the fmt layer) — a separate `set_global_default` would fail with `SetGlobalDefault`.
- Tests that capture probes should be `#[serial]` (via `serial_test`) so concurrent runs do not pollute the shared recorder.
- **Activation**: probe sites compile to no-ops unless `kithara-stream/usdt-probes` and/or `kithara-hls/usdt-probes` are enabled in the test build.
