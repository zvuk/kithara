<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-analysis.svg)](https://crates.io/crates/kithara-analysis)
[![docs.rs](https://docs.rs/kithara-analysis/badge.svg)](https://docs.rs/kithara-analysis)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-analysis

Progressive per-track waveform and beat analysis over decoded source audio.
It accepts both a dedicated `AudioReader` and already-decoded chunks through
`AudioObserver`, so playback warms the same analysis pass without decoding its
source twice. It owns analysis state, scheduling, DSP, and pure versioned bytes;
the source reader remains in `kithara-audio` and caching results is the
application's job.

## Key Types

- `AnalyzerBuilder` / `AnalysisWorkerConfig` / `AnalysisWorker` — configure
  analysis and run progressive per-track passes on a domain dispatcher.
- `AnalysisProducer` — non-blocking decoded-chunk ingress for an open pass.
- `TrackAnalysis` — self-contained published snapshot: token, revision, source
  axis, coverage, fingerprint, waveform, and beat artifact.
- `Waveform` / `BeatArtifact` — analysis artifacts with versioned byte codecs.
- `BlobError` - format, corruption, and pooled restore errors from the artifact
  and composite byte codecs.

## Features

- `analysis-waveform` — three-band waveform analyzer.
- `analysis-beat` — beat-analysis pass; pick a detector with `beat-dsp`
  (no model) or `beat-nn` (neural network). `beat-nn-small`, `beat-nn-full`,
  `beat-nn-full-int8` embed a model; `beat-cuda` runs it on CUDA.
- `symphonia` (default), `apple`, `android`, `webcodecs`, `fdk-aac` — decoder
  for the reader this crate opens.
- `client-reqwest` (default), `client-wreq`, `tls-rustls` (default),
  `tls-native` — HTTP and TLS.
- `mock`, `usdt` — test mocks and USDT probes.

## Integration

Reads decoded audio through `kithara-audio`'s `AudioReader` or observes the
chunks playback already decodes (`AudioObserver`). Scratch buffers come from
the caller's `PoolRegion`, so analysis shares the application's memory budget.
Storing a `TrackAnalysis`, its cache key, and eviction are up to the
application; `write_to` serializes it. With `beat-cuda`, pass the caller-owned
ONNX Runtime environment through `BeatAnalysisConfig::builder().cuda_environment(...)`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-analysis) for the scheduling, ingest, waveform, and codec
contracts.
