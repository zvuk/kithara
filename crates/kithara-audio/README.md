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
conversion, and source readiness. `Audio::prepare` returns an `Audio<S>` that
owns the open source and decoder. Its owning lane drives reads and synchronous
seeks on one thread; the PCM ring and segment tags belong to that lane, not this
crate. Host-rate changes rebuild the decoder without reopening the source.

## Usage

```rust
use kithara_audio::{
    Audio, AudioConfig, AudioDecoderConfig, DecoderResamplerSettings, ResamplerQuality,
};
use kithara_decode::GaplessMode;

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

let mut audio = Audio::prepare(audio_config, worker_wake, pools).await?;
let activity_writer = audio.take_activity_writer();
```

## Key Types

- `Audio<S>` — open decoded source; the owning lane reads frames, seeks, and
  sets its host sample rate.
- `AudioConfig<T>` — `bon` builder for stream config, decode backend,
  decoder-owned resampling, gapless mode, source readiness, and events.
- `AudioSource` — worker-independent per-track decoded-audio source contract.
- `Activity` / `ActivityWriter` (from `kithara-stream`) — read-only loader
  snapshot and its move-only publisher, transferred once to the owning lane
  through `Audio::take_activity_writer`.
- `ResamplerQuality` / `ResamplerOptions` — sample-rate-conversion config
  threaded into the decoder-owned resampler plan.

## Features

- `symphonia` (default), `apple`, `apple-fused-src`, `android`, `webcodecs`,
  `fdk-aac` — decoders, forwarded to `kithara-decode`.
- `resample-rubato` (default), `resample-glide` — sample-rate converters.
- `client-reqwest` (default), `client-wreq`, `tls-rustls` (default),
  `tls-native` — HTTP and TLS, forwarded to network crates.
- `mock`, `usdt`, `perf`, `memprof` — test mocks, USDT probes, `hotpath`
  timing, allocation tracking for profiling examples.

## Integration

Sits between `kithara-decode` and the player: it consumes `kithara-stream`
sources and produces decoded chunks whose types come from `kithara-signal`.
Tempo, effects, and output belong to `kithara-play` and below; waveform and
beat analysis to `kithara-analysis`, which reads this crate's decoded source.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-audio) for detailed threading, seek/recreate, and
prepared-source contracts. Source analysis contracts are in
[`kithara-analysis`](https://github.com/zvuk/kithara/wiki/kithara-analysis).
