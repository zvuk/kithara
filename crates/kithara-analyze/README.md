<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-analyze

Analyze a batch of audio tracks and export one record per track, as JSON or as
a binary analysis document. Sources open through Kithara's file and HLS loaders at their decoded native sample rate.
The default analysis includes waveform, beat and musical key.

```sh
cargo run -p kithara-analyze -- /music/one.flac https://host/two.mp3
cargo run -p kithara-analyze -- --artifacts beat,key --jobs 4 --out ./analysis - < tracks.ndjson
cargo run -p kithara-analyze -- --waveform-fold 10 /music/one.flac
cargo run -p kithara-analyze -- --format binary --out ./analysis /music/one.flac
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
is a positive integer, 1 by default: factor 10 folds ten native waveform columns
into each bucket. `--format json` (default) or `--format binary` selects the
record encoding; one run writes one format, and any other value is a usage
error. `--format binary` requires `--out`: a binary record has no line
delimiter.

Without `--out`, stdout contains one JSON line per record and diagnostics go to
stderr. `--out DIR` writes each record to a same-directory temporary file,
flushes it, and publishes the completed file as `<id>.json` (or `<id>.analysis`
in the binary format) without replacing an
occupied destination. A conflict preserves the occupied destination and exits 1.
On Unix paths that cannot use no-replace rename, publication uses a hard link
followed by temporary-name cleanup; cleanup failure or interruption can leave an
additional temporary hard link to the same completed file. Analysis records contain `id`,
`src`, `status`,
`tool_version`, artifact `fingerprint` tags, the `source` frame axis, `coverage`,
`missing`, `beat`, `grid`, `grid_unavailable`, `key` and `waveform`. Markers use
source frames; the grid uses media seconds. Waveform records include the bucket
span in source frames and four companded band heights per side: low, low_mid,
high_mid and high. Each bucket is `[left, right]`, each side an array of four
heights in band order; at 48 kHz the default column spans 64 frames. Absent
artifacts are null.

Failed records carry `id`, `src`, `status: "failed"` and `error`. Cancelled
records carry `id`, `src` and `status: "cancelled"`.

A binary record is the track's `TrackAnalysis` snapshot exactly as `write_to`
encodes it, with the track identifier as its token; read it with
`TrackAnalysis::try_from(&[u8])`. Nothing else marks an incomplete pass: the
read snapshot is settled, `is_complete()` is false and `missing()` returns the
ranges the JSON record lists as `missing`. A failed or cancelled track writes
no binary file; instead a warning on stderr names its `id`, `src`, `status` and,
for a failure, the `cause`.

SIGINT cancels in-flight tracks, preserves written records and leaves queued
tracks without records.
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
