# kithara-events — Context

Detailed contracts and invariants for the kithara-events crate; the README is the overview.

## Features

All variants of `Event` and all subsystem sub-enums are feature-gated. The default set turns everything on so consumers get the full event surface; disable defaults and pick a la carte for smaller builds.

<table>
<tr><th>Feature</th><th>Default</th><th>Enables</th></tr>
<tr><td><code>abr</code></td><td>yes</td><td><code>AbrEvent</code>, <code>AbrMode</code>, <code>VariantInfo</code>, …</td></tr>
<tr><td><code>app</code></td><td>yes</td><td><code>AppEvent</code></td></tr>
<tr><td><code>audio</code></td><td>yes</td><td><code>AudioEvent</code>, <code>AudioFormat</code>, <code>SeekLifecycleStage</code></td></tr>
<tr><td><code>downloader</code></td><td>yes</td><td><code>DownloaderEvent</code>, <code>CancelReason</code>, <code>RequestId</code> (pulls <code>kithara-net</code>)</td></tr>
<tr><td><code>file</code></td><td>yes</td><td><code>FileEvent</code>, <code>FileError</code></td></tr>
<tr><td><code>hls</code></td><td>yes</td><td><code>HlsEvent</code>, <code>HlsError</code> (implies <code>abr</code>)</td></tr>
<tr><td><code>player</code></td><td>yes</td><td><code>PlayerEvent</code>, <code>EngineEvent</code>, <code>ItemEvent</code>, <code>SessionEvent</code>, <code>TransportEvent</code>, <code>SyncEvent</code>, <code>DjEvent</code>, <code>MediaTime</code>, ...</td></tr>
<tr><td><code>queue</code></td><td>yes</td><td><code>QueueEvent</code>, <code>TrackId</code>, <code>TrackStatus</code></td></tr>
<tr><td><code>client-reqwest</code></td><td>no</td><td>Forward the reqwest HTTP backend to optional <code>kithara-net</code></td></tr>
<tr><td><code>client-wreq</code></td><td>no</td><td>Forward the wreq HTTP backend to optional <code>kithara-net</code></td></tr>
<tr><td><code>tls-rustls</code></td><td>no</td><td>Forward rustls TLS selection to optional <code>kithara-net</code></td></tr>
<tr><td><code>tls-native</code></td><td>no</td><td>Forward native TLS selection to optional <code>kithara-net</code></td></tr>
</table>

## Trait Bridges

- `{Downloader,Hls,File,Audio,Player,Engine,Item,Session,Transport,Sync,Dj,App,Queue,Abr}Event` -> `Event` (`From`) - lift subsystem events into the top-level enum
- `TrackId` ↔ `u64` (`From` both ways) — track-id newtype conversions
- `AbrMode` ↔ `usize` (`From` both ways) — variant-index encoding
- `Duration` → `MediaTime` (`From`) / `&MediaTime` → `Duration` (`TryFrom`) — playback-time bridge, rejects invalid/indefinite
- `FileError` / `HlsError` / `AudioFormat` / `TrackId` (`Display`) — human-readable rendering

`PlaybackDirection` is intentionally exhaustive: forward and reverse are the
complete signed directions used by track-coordinate math.

## Integration

Used by `kithara-audio`, `kithara-file`, `kithara-hls`, `kithara-abr`, `kithara-play`, `kithara-queue`, `kithara-app`, and the `kithara` facade. Each subsystem publishes to a shared `EventBus`; consumers subscribe for a unified `Event` stream via `tokio::sync::broadcast`.
