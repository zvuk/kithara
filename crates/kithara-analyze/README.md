<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-analyze

Analyze a batch of audio tracks and export one JSON record per track. Sources
open through Kithara's file and HLS loaders at their decoded native sample rate.
The default analysis includes waveform, beat and musical key.

```sh
cargo run -p kithara-analyze -- /music/one.flac https://host/two.mp3
cargo run -p kithara-analyze -- --artifacts beat,key --jobs 4 --out ./analysis - < tracks.ndjson
cargo run -p kithara-analyze -- --waveform-fold 10 /music/one.flac
```

Paths and URLs use the file stem as their identifier. With `-`, standard input
contains one object per line:

```json
{"id":"one","src":"/music/one.flac"}
{"id":"two","src":"https://host/two.mp3"}
```

Identifiers must be distinct single file names. All input is validated before
analysis starts. `--jobs` defaults to the available processor count and limits
concurrent source loads and passes. Records are written in completion order;
one job processes the input in order.

`--artifacts waveform,beat,key` selects the analyzers to build. `--waveform-fold`
is a positive integer, 1 by default: factor 10 folds ten native waveform windows
into each bucket. `--format json` selects the only current output format.

Without `--out`, stdout contains one JSON line per record and diagnostics go to
stderr. `--out DIR` writes `<id>.json` through a temporary file in that directory
and an atomic rename. Analysis records contain `id`, `src`, `status`,
`tool_version`, artifact `fingerprint` tags, the `source` frame axis, `coverage`,
`missing`, `beat`, `grid`, `grid_unavailable`, `key` and `waveform`. Markers use
source frames; the grid uses media seconds. Waveform records include the bucket
span in source frames and low/mid/high band heights. Absent artifacts are null.

Failed records carry `id`, `src`, `status: "failed"` and `error`. Cancelled
records carry `id`, `src` and `status: "cancelled"`. SIGINT cancels in-flight
tracks, preserves written records and leaves queued tracks without records.
Exit codes are 0 for complete/incomplete records, 1 for failure or cancellation,
and 2 for invalid input or options.

Default features are `beat-nn-small`, `symphonia` and `fdk-aac`. Alternative beat
features are `beat-nn-full`, `beat-nn-full-int8` and `beat-dsp`. A server build
selects the full model explicitly:

```sh
cargo build -p kithara-analyze --release --no-default-features --features beat-nn-full,symphonia,fdk-aac
```

The beat fingerprint identifies the selected model.

The [kithara-analysis contract](https://github.com/zvuk/kithara/wiki/kithara-analysis)
owns analysis snapshots, fingerprints and terminal outcomes.
