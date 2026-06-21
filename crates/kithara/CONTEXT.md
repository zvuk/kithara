# kithara — Context

Detailed contracts and invariants for the kithara crate; the README is the overview.

## Architecture

```mermaid
%%{init: {"flowchart": {"curve": "linear"}} }%%
flowchart LR
    RC[ResourceConfig] -->|auto-detect| R[Resource]
    R -->|".m3u8"| AH["Audio‹Stream‹Hls››"]
    R -->|other| AF["Audio‹Stream‹File››"]
    AH --> PR["Box‹dyn PcmReader›"]
    AF --> PR
    PR -->|"read / seek"| APP[Your audio callback]
```

PCM flows straight from the decoder to your callback through `read()`. The
optional `EventBus` (`resource.event_bus()`) is a side-channel for
observability — decode progress, buffering, HLS variant switches — and never
sits in the audio path.

## Features

<table>
<tr><th>Feature</th><th>Default</th><th>Enables</th></tr>
<tr><td><code>file</code></td><td>yes</td><td>Progressive pipeline (<code>kithara-file</code>, <code>kithara-assets</code>, <code>kithara-net</code>)</td></tr>
<tr><td><code>hls</code></td><td>yes</td><td>HLS pipeline (<code>kithara-hls</code>, <code>kithara-abr</code>, <code>kithara-assets</code>, <code>kithara-net</code>, <code>kithara-drm</code>)</td></tr>
<tr><td><code>symphonia</code></td><td>yes</td><td>Symphonia software decoder (<code>kithara-audio/symphonia</code>, <code>kithara-decode/symphonia</code>)</td></tr>
<tr><td><code>apple</code></td><td>no</td><td>Apple AudioToolbox hardware decoder (<code>kithara-audio/apple</code>, <code>kithara-play/apple</code>)</td></tr>
<tr><td><code>android</code></td><td>no</td><td>Android <code>MediaCodec</code> hardware decoder (<code>kithara-audio/android</code>, <code>kithara-decode/android</code>)</td></tr>
<tr><td><code>assets</code></td><td>no</td><td>Asset/storage modules (<code>kithara-assets</code>, <code>kithara-storage</code>)</td></tr>
<tr><td><code>net</code></td><td>no</td><td>Network module (<code>kithara-net</code>)</td></tr>
<tr><td><code>bufpool</code></td><td>no</td><td>Aggregator flag used by <code>full</code>; the <code>kithara::bufpool</code> module is always re-exported</td></tr>
<tr><td><code>full</code></td><td>no</td><td>Shortcut for <code>file + hls + assets + net + bufpool</code></td></tr>
<tr><td><code>probe</code></td><td>no</td><td>USDT probes across <code>kithara-audio</code>, <code>kithara-decode</code>, <code>kithara-stream</code>, <code>kithara-play</code></td></tr>
<tr><td><code>mock</code></td><td>no</td><td><code>unimock</code>-generated mocks across the same sub-crates</td></tr>
<tr><td><code>perf</code></td><td>no</td><td>Hotpath instrumentation across sub-crates</td></tr>
</table>

## Key Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>Resource</code></td><td>Type-erased <code>Box&lt;dyn PcmReader&gt;</code> — the single entry point for PCM reads</td></tr>
<tr><td><code>ResourceConfig</code></td><td>Builder for source, network, ABR, decoder backend, and cache options</td></tr>
<tr><td><code>ResourceSrc</code></td><td>Source: <code>Url(Url)</code> or <code>Path(PathBuf)</code></td></tr>
<tr><td><code>SourceType</code></td><td>Auto-detection result: <code>HlsStream(Url)</code>, <code>RemoteFile(Url)</code>, or <code>LocalFile(PathBuf)</code></td></tr>
<tr><td><code>ReadOutcome</code></td><td>Result of a read: <code>Frames { count, position }</code>, <code>Pending { reason, position }</code>, or <code>Eof { position }</code></td></tr>
<tr><td><code>EventBus</code></td><td>Broadcast publisher for the unified <code>Event</code> stream (observability only)</td></tr>
</table>

## Re-exports

Each engine layer is re-exported as a module: `kithara::audio`, `kithara::bufpool`,
`kithara::decode`, `kithara::events`, `kithara::platform`, `kithara::play`,
`kithara::stream`. The `file`/`hls`/`assets`/`net`/`storage` modules are
feature-gated. For advanced control — multi-slot engine, crossfade, EQ — reach
into `kithara::play` (`Engine`, `Player`, `CrossfadeConfig`, `Equalizer`). The
`prelude` collects the everyday types.

## Integration

Most consumers depend on `kithara` with default features and call
`Resource::new(ResourceConfig::new(url)?).await?`. For wasm or embedded builds,
disable defaults and pick a minimal feature set (e.g. `file` + `symphonia`).
