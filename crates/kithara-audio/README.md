<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
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
graph TB
    subgraph "Main / Consumer Thread"
        App["Application Code"]
        Resource["Resource / Audio&lt;S&gt;<br/><i>PcmReader interface</i>"]
        App -- "read(buf)" --> Resource
    end

    subgraph "tokio Runtime (async tasks)"
        NetTask["Network I/O<br/><i>reqwest + retry</i>"]
        WriterTask["Writer&lt;E&gt;<br/><i>byte pump</i>"]
        EventFwd["Event Forwarder<br/><i>broadcast relay</i>"]
    end

    subgraph "rayon ThreadPool (blocking)"
        DLLoop["Backend::run_downloader<br/><i>orchestration loop</i>"]
        DecodeWorker["AudioWorker<br/><i>decode + effects</i>"]
    end

    subgraph "Shared State (lock-based)"
        StorageRes["StorageResource<br/><i>Mutex + Condvar</i>"]
        SegIdx["SegmentIndex / SharedSegments<br/><i>Mutex + Condvar</i>"]
        Progress["Progress<br/><i>AtomicU64 + Notify</i>"]
    end

    subgraph "Channels"
        PcmChan["kanal::bounded&lt;PcmChunk&gt;<br/><i>decode -> consumer</i>"]
        CmdChan["kanal::bounded&lt;AudioCommand&gt;<br/><i>consumer -> worker</i>"]
        EventChan["broadcast::channel&lt;Event&gt;<br/><i>all -> consumer</i>"]
    end

    DLLoop -- "plan + fetch" --> NetTask
    NetTask -- "bytes" --> WriterTask
    WriterTask -- "write_at()" --> StorageRes
    DLLoop -- "commit()" --> SegIdx

    DecodeWorker -- "wait_range() blocks" --> StorageRes
    DecodeWorker -- "read_at()" --> StorageRes
    DecodeWorker -- "PcmChunk" --> PcmChan
    Resource -- "recv()" --> PcmChan

    Resource -- "Seek cmd" --> CmdChan
    CmdChan --> DecodeWorker

    EventFwd -- "ResourceEvent" --> EventChan
    EventChan --> App

    DLLoop -- "should_throttle()" --> Progress
    DecodeWorker -- "set_read_pos()" --> Progress

    style StorageRes fill:#d4a574,color:#000
    style SegIdx fill:#d4a574,color:#000
    style Progress fill:#d4a574,color:#000
    style PcmChan fill:#5b8a5b,color:#fff
    style CmdChan fill:#5b8a5b,color:#fff
    style EventChan fill:#5b8a5b,color:#fff
```

- **OS thread** (`kithara-audio`): runs `run_audio_loop` -- drains seek commands, calls `Decoder::next_chunk`, applies effects (resampler), sends processed chunks through a bounded `kanal` channel with backpressure.
- **tokio task**: forwards stream events (ABR switch, progress) into a unified `AudioPipelineEvent` broadcast channel.
- **Epoch-based invalidation**: after seek, stale in-flight chunks are filtered by epoch counter (`Arc<AtomicU64>`).

## Pipeline Architecture

```
Stream<T> (Read + Seek)
  → DecoderFactory creates Box<dyn InnerDecoder>
    → StreamAudioSource (format change detection, effects chain)
      → AudioWorker (blocking thread, command handling)
        → kanal channel (bounded, backpressure)
          → Audio<S> (consumer: PcmReader, Iterator, rodio::Source)
```

## Resampler Quality Levels

| Quality | Algorithm | Use Case |
|---------|-----------|----------|
| Fast | Polynomial (cubic) | Low-power, previews |
| Normal | 64-tap sinc, linear | Standard playback |
| Good | 128-tap sinc, linear | Better quality |
| High (default) | 256-tap sinc, cubic | Recommended for music |
| Maximum | FFT-based | Offline / high-end |

## Format Change Handling

On ABR variant switch, `StreamAudioSource` detects the format change via `media_info()` polling, then:

1. Uses the variant fence to prevent cross-variant reads.
2. Seeks to the first segment of the new variant (where init data lives).
3. Recreates the decoder via factory.
4. Resets the effects chain to avoid audio artifacts.

## Epoch-Based Seek

On seek, epoch is incremented atomically. The worker tags each decoded chunk with the current epoch. The consumer discards stale chunks (old epoch), preventing leftover data from reaching output after a seek.

## Integration

Sits between `kithara-decode` (synchronous Symphonia wrapper) and the consumer (rodio, cpal, custom). Depends on `kithara-stream` for `Stream<T>` and `kithara-bufpool` for zero-allocation PCM buffers.
