<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-audio

Audio pipeline with decoding, effects chain, and sample rate conversion. Runs a dedicated OS thread for blocking decode/process work and bridges it to the caller via `kanal` channels. Provides `Audio<S>` as the main entry point and `AudioSyncReader` (behind `rodio` feature) for rodio integration.

## Usage

```rust
use kithara_audio::{Audio, AudioConfig, ResamplerQuality};
use kithara_hls::{Hls, HlsConfig};
use kithara_stream::Stream;

let config = AudioConfig::<Hls>::new(hls_config)
    .with_host_sample_rate(sample_rate)
    .with_resampler_quality(ResamplerQuality::High);
let mut audio = Audio::<Stream<Hls>>::new(config).await?;

// Read interleaved PCM
let mut buf = [0.0f32; 1024];
while !audio.is_eof() {
    let n = audio.read(&mut buf);
    play(&buf[..n]);
}
```

## Threading model

```mermaid
sequenceDiagram
    participant Caller as Caller thread
    participant Worker as OS thread (kithara-audio)
    participant Decoder as Decoder (Symphonia)
    participant Effects as Effects (resampler)
    participant Events as Event forward (tokio)

    Caller->>Worker: Audio::new() spawns thread

    loop Worker loop
        Worker->>Worker: drain seek commands
        Worker->>Decoder: next_chunk()
        Decoder-->>Worker: PcmChunk
        Worker->>Effects: process(chunk)
        Effects-->>Worker: resampled PcmChunk
        Worker--)Caller: kanal send (bounded, backpressure)
    end

    Caller->>Worker: seek(position, epoch++)
    Worker->>Decoder: seek + reset effects
    Note over Worker,Caller: stale in-flight chunks filtered by epoch

    Events--)Caller: AudioPipelineEvent (broadcast)
```

- **OS thread** (`kithara-audio`): runs `run_audio_loop` -- drains seek commands, calls `Decoder::next_chunk`, applies effects (resampler), sends processed chunks through a bounded `kanal` channel with backpressure.
- **tokio task**: forwards stream events (ABR switch, progress) into a unified `AudioPipelineEvent` broadcast channel.
- **Epoch-based invalidation**: after seek, stale in-flight chunks are filtered by epoch counter (`Arc<AtomicU64>`).

## Integration

Sits between `kithara-decode` (synchronous Symphonia wrapper) and the consumer (rodio, cpal, custom). Depends on `kithara-stream` for `Stream<T>` and `kithara-bufpool` for zero-allocation PCM buffers.
