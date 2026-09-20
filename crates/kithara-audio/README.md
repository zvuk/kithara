<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-audio.svg)](https://crates.io/crates/kithara-audio)
[![docs.rs](https://docs.rs/kithara-audio/badge.svg)](https://docs.rs/kithara-audio)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-audio

Decoded-audio source pipeline with decoder lifecycle, decoder-owned sample-rate
conversion, and source readiness. `Audio<S>` is the audio reader surface.
`Audio::prepare` returns the concrete `AudioSource` and
worker-neutral `PreparedAudioLane`; `kithara-play` owns `PlayWorker`, the
per-track node, final output admission, playback effects, and engine-load
measurement, while `kithara-warp` owns the resident Warp renderer.

## Usage

```rust
use kithara_audio::{
    AudioConfig, AudioDecoderConfig, DecoderResamplerSettings, ResamplerQuality,
};
use kithara_decode::GaplessMode;
use kithara_play::{PlayWorker, PlayWorkerConfig};

let decoder_config = AudioDecoderConfig::builder()
    .gapless_mode(GaplessMode::CodecPriming)
    .resampler(
        DecoderResamplerSettings::builder()
            .quality(ResamplerQuality::High)
            .build(),
    )
    .build();
let audio_config = AudioConfig::for_stream(hls_config)
    .host_sample_rate(sample_rate)
    .decoder(decoder_config)
    .build();

let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
let mut audio = worker.open(audio_config).await?;
```

## Key Types

- `Audio<S>` — main audio reader; the consumer reads frames from it and requests
  seeks.
- `AudioConfig<T>` — `bon` builder for stream config, decode backend,
  decoder-owned resampling, gapless mode, source readiness, and events.
- `AudioSource` — worker-independent per-track decoded-audio source contract.
- `PreparedAudio` / `PreparedAudioLane` — reader plus the still-concrete producer
  seam consumed by `kithara-play`.
- `ResamplerQuality` / `ResamplerOptions` — sample-rate-conversion config
  threaded into the decoder-owned resampler plan.

## Features

<table>

<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>

<tr><td><code>default</code></td><td>yes</td><td><code>symphonia</code> + <code>resample-rubato</code> + <code>client-reqwest</code> + <code>tls-rustls</code></td></tr>

<tr><td><code>symphonia</code></td><td>yes</td><td>Symphonia software decoder path via <code>kithara-decode/symphonia</code></td></tr>

<tr><td><code>resample-rubato</code></td><td>yes</td><td>Rubato sample-rate conversion via <code>kithara-resampler/resample-rubato</code>; <code>resample-glide</code> selects the scalar backend instead</td></tr>

<tr><td><code>client-reqwest</code></td><td>yes</td><td>Forward the default HTTP backend selection to network-reaching deps</td></tr>

<tr><td><code>tls-rustls</code></td><td>yes</td><td>Forward rustls TLS selection to network-reaching deps</td></tr>

<tr><td><code>apple</code></td><td>no</td><td>Apple AudioToolbox hardware decoder via <code>kithara-decode/apple</code></td></tr>

<tr><td><code>android</code></td><td>no</td><td>Android <code>MediaExtractor</code>/<code>MediaCodec</code> via <code>kithara-decode/android</code></td></tr>

<tr><td><code>fdk-aac</code></td><td>no</td><td>Enable libfdk-aac HE-AAC v1/v2 decode in the software path</td></tr>

<tr><td><code>client-wreq</code></td><td>no</td><td>Forward the native <code>wreq</code> HTTP backend selection to network-reaching deps</td></tr>

<tr><td><code>tls-native</code></td><td>no</td><td>Forward native TLS selection to network-reaching deps</td></tr>

<tr><td><code>usdt</code></td><td>no</td><td>USDT probes for tracing</td></tr>

<tr><td><code>mock</code></td><td>no</td><td>Generated mocks for tests</td></tr>

<tr><td><code>perf</code></td><td>no</td><td>Hotpath timing instrumentation</td></tr>

<tr><td><code>memprof</code></td><td>no</td><td>Allocation tracking for profiling examples</td></tr>

</table>

## Integration

`kithara-audio` sits between `kithara-decode` and playback consumers. The
downloader lives in `kithara-stream`; audio consumes stream/storage contracts
without reconstructing protocol policy. `kithara-signal` owns `AudioSpec`,
`AudioChunkInfo`, `AudioChunk`, and pure sample/time math, while
`kithara-bufpool` owns their pooled sample storage. This crate owns the runtime
`AudioReader` and `AudioSource` protocols around those values. `kithara-play` composes the prepared
source with `kithara-warp` and `kithara-stretch`; none of those playback
transforms are owned here. `kithara-analysis` consumes this crate's decoded
source and observer protocols; analysis itself is not owned here.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-audio) for detailed threading, seek/recreate, and
prepared-source contracts. Source analysis contracts are in
[`kithara-analysis`](https://github.com/zvuk/kithara/wiki/kithara-analysis).
