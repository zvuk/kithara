<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-audio.svg)](https://crates.io/crates/kithara-audio)
[![docs.rs](https://docs.rs/kithara-audio/badge.svg)](https://docs.rs/kithara-audio)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-audio

Audio pipeline with decoding, effects, resampling, optional time-stretch, and
source-signal analysis. `Audio<S>` is the PCM reader surface; an
`AudioWorkerHandle` runs decode/effects work on a shared OS thread and hands
processed chunks to the caller through lock-free rings.

## Features

<table>
<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>
<tr><td><code>default</code></td><td>yes</td><td><code>symphonia</code> + <code>stretch-signalsmith</code> + <code>client-reqwest</code> + <code>tls-rustls</code></td></tr>
<tr><td><code>symphonia</code></td><td>yes</td><td>Symphonia software decoder path via <code>kithara-decode/symphonia</code></td></tr>
<tr><td><code>stretch-signalsmith</code></td><td>yes</td><td>Native <code>signalsmith-stretch</code> key-lock backend through <code>kithara-stretch</code></td></tr>
<tr><td><code>client-reqwest</code></td><td>yes</td><td>Forward the default HTTP backend selection to network-reaching deps</td></tr>
<tr><td><code>tls-rustls</code></td><td>yes</td><td>Forward rustls TLS selection to network-reaching deps</td></tr>
<tr><td><code>apple</code></td><td>no</td><td>Apple AudioToolbox hardware decoder via <code>kithara-decode/apple</code></td></tr>
<tr><td><code>android</code></td><td>no</td><td>Android <code>MediaExtractor</code>/<code>MediaCodec</code> via <code>kithara-decode/android</code></td></tr>
<tr><td><code>fdk-aac</code></td><td>no</td><td>Enable libfdk-aac HE-AAC v1/v2 decode in the software path</td></tr>
<tr><td><code>beat-nn</code></td><td>no</td><td>Enable NN beat/downbeat analysis through <code>kithara-beat</code></td></tr>
<tr><td><code>stretch-bungee</code></td><td>no</td><td>Native <code>bungee-rs</code> key-lock backend through <code>kithara-stretch</code></td></tr>
<tr><td><code>client-wreq</code></td><td>no</td><td>Forward the native <code>wreq</code> HTTP backend selection to network-reaching deps</td></tr>
<tr><td><code>tls-native</code></td><td>no</td><td>Forward native TLS selection to network-reaching deps</td></tr>
<tr><td><code>probe</code></td><td>no</td><td>USDT probes for tracing</td></tr>
<tr><td><code>mock</code></td><td>no</td><td>Generated mocks for tests</td></tr>
<tr><td><code>perf</code></td><td>no</td><td>Hotpath timing instrumentation</td></tr>
<tr><td><code>memprof</code></td><td>no</td><td>Allocation tracking for profiling examples</td></tr>
</table>

## Key Types

- `Audio<S>` — main PCM reader; the consumer reads frames from it and requests
  seeks.
- `AudioConfig<T>` — `bon` builder for stream config, decode backend,
  resampling, gapless mode, stretch controls, worker handle, and engine load.
- `AudioWorkerHandle` / `AudioWorkerSource` — shared worker thread handle and
  per-track source contract.
- `ResamplerQuality` / `ResamplerOptions` — sample-rate-conversion config
  threaded into the decoder-owned resampler plan.
- `StretchControls` / `TimeStretchProcessor` — preserve-pitch tempo mode when a
  `kithara-stretch` backend is compiled.
- `AnalyzerBuilder` / `AnalysisWorker` / `TrackAnalysis` — source-signal
  waveform and optional beat analysis.
- `TrackBeatMap` / `TrackBeat` / `SourceFrame` — validated musical mapping from
  analysed beat markers to an explicit host-rate source axis.
- `SourceRange` / `SourceRangeRequest` — typed bounded reads from the canonical
  post-gapless, host-rate `Audio` ring before playback effects.
- `Waveform` / `BeatGrid` — analysis artifacts; public blob I/O uses
  `Vec::<u8>::from(&artifact)` and `Artifact::try_from(&[u8])`.
- `EngineLoad` / `EngineLoadSnapshot` — live decode/effects cost meter.

## Usage

```rust
use kithara_audio::{
    Audio, AudioConfig, AudioDecoderConfig, DecoderResamplerSettings, ResamplerQuality,
};
use kithara_decode::GaplessMode;
use kithara_hls::{Hls, HlsConfig};
use kithara_stream::Stream;

let decoder_config = AudioDecoderConfig::builder()
    .gapless_mode(GaplessMode::CodecPriming)
    .resampler(
        DecoderResamplerSettings::builder()
            .quality(ResamplerQuality::High)
            .build(),
    )
    .build();
let audio_config = AudioConfig::<Hls>::builder()
    .stream(hls_config)
    .host_sample_rate(sample_rate)
    .decoder(decoder_config)
    .build();

let mut audio = Audio::<Stream<Hls>>::new(audio_config).await?;
```

## Orientation

`kithara-audio` sits between `kithara-decode` and playback consumers. The
downloader lives in `kithara-stream`; audio consumes stream/storage contracts
without reconstructing protocol policy. Time-stretch DSP backends live in
`kithara-stretch` and are re-exported only when a stretch feature is compiled.
Analysis runs on decoded source PCM, not post-EQ, post-stretch, or post-resample
output.

See [CONTEXT.md](CONTEXT.md) for detailed threading, seek/recreate, analysis,
blob, and time-stretch contracts.
