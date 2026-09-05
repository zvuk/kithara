# kithara-broadcast — Context

Contracts and invariants for the kithara-broadcast crate; the README is the overview.

The crate turns `kithara-encode` access units into HLS media segments and serves them: a packaging core with no clock of its own, and a native service that drives it from a bounded `LiveOutput` and publishes it over HTTP.

The native encoder it opens is `kithara-encode`'s fdk-aac AAC-LC backend, asked for by name. That backend builds from vendored sources into the binary, so a broadcast needs no FFmpeg on the machine it runs on and none in a shipped application. libfdk's vendored `FDK_archdef.h` has no branch for MSVC on ARM64, so the service cannot be built for `aarch64-pc-windows-msvc`. On wasm32 the portable `Segmenter` and `LiveWindow` remain available while the native encoder, worker intake, and HTTP origin are absent.

## Configuration

`BroadcastConfig` is the sole construction and knob surface. Its Bon builder takes the shared `Worker` and typed `PoolRegion`, and the config also owns the optional cancellation parent, encoding profile, PCM and dispatcher capacities, tick budget, scheduler policy, retention, bind address, and graceful-stop timeout. `Segmenter`, `LiveWindow`, and `Broadcast::start` all take that same config, so the sample rate is the media timescale everywhere and no caller can pair a segmenter with a window that disagrees about time. The app copies it with only the Host-measured sample rate changed. `BroadcastConfigPatch` is what a configuration document may say about it: the shared `Worker` and `PoolRegion` it is built from, the cancellation parent, the codec and container profile, the scheduler priority, and the sample rate are not document keys — the first three exist only at the construction site, `validate` admits one codec profile, `Priority` carries no `Deserialize`, and the packager overwrites the rate with the measured master format.

AAC-LC/ADTS is the default and currently the only HLS profile. Any other configured codec/container pair returns `UnsupportedProfile`; there is no fallback.

`validate` rejects zero audio, a zero window, a segment target shorter than one media tick, and — per RFC 8216 §6.2.2 — a window that spans fewer than three target durations. A short window is a typed `PlaylistTooShort`, never a silent adjustment.

That check covers segments the segmenter cut at the target. A window made of segments an intake gap cut short can still span less than three target durations, because the announced `EXT-X-TARGETDURATION` stays what a client was first told: a constant target duration is the rule a reloading client depends on, and the span recovers as full segments slide in.

## ADTS framing

Every access unit gets a 7-byte ADTS header (no CRC): MPEG-4, layer 0, `protection_absent`, AAC-LC profile, `buffer_fullness = 0x7FF` (VBR), one raw data block. Each frame is a sync point, so segments are byte-concatenatable and a client joining at any segment decodes it standalone.

The header's `sampling_frequency_index` and `channel_configuration` fields are fixed at construction, so audio ADTS cannot describe is rejected there: a sample rate outside the 13-entry table (96000 … 7350 Hz) errors, and so does a channel count outside 1..=6 — `channel_configuration` counts channels only up to 6, and its remaining value stands for 7.1, which is a layout rather than a count.

`kithara-decode` builds the same header shape for its fdk-aac transport (`symphonia/aac_fdk.rs`). The two stay separate: neither crate depends on the other, and a shared owner for seven bytes would couple the decode path to the broadcast path.

## Packed-audio timestamp

RFC 8216 §3.4 requires every packed-audio segment to open with an ID3v2 tag whose single PRIV frame is owned by `com.apple.streaming.transportStreamTimestamp` and whose 8-octet big-endian body is the first sample's MPEG-2 timestamp. `TimestampTag` renders that fixed-size tag and `Segmenter` prepends it when it closes a segment, from the stream time the segment started at.

`TimestampTag::mpeg_timestamp` is the only place the media timescale and the 90 kHz MPEG-2 domain meet: it rounds to the nearest 90 kHz tick and wraps at 33 bits. Everything else in the crate counts in the configured sample rate.

The stream clock counts encoded audio only. `mark_drop` closes a segment and marks the next one discontinuous; it does not synthesise a gap in the timestamps, because `EXT-X-DISCONTINUITY` is what tells a client the timeline broke. The encoder runs across the gap and holds its own delay - two access units of priming - when the cut lands, so the discontinuous segment opens with the last of what came ahead of the gap. A final reported gap leaves that delay in the tail because `finish` drains the encoder before the window ends.

symphonia's ADTS reader resyncs to the next sync word on every frame, so the prefix costs the in-tree decode path nothing.

## Segment rotation

`Segmenter` runs on the media clock: it sums the `duration` of the access units it framed, in the timescale those durations are expressed in. `kithara-encode` guarantees the durations tile the pts timeline, so the sum is the segment's exact media length.

Rotation is append-then-check: the access unit whose duration carries the accumulated total to the target belongs to the segment it closes. A segment therefore runs slightly past the target rather than short of it, and segment durations sum to the pushed audio exactly.

Sequence numbers count emitted segments from zero. An empty buffer emits nothing, so a close with no framed audio consumes no sequence number.

`mark_drop` is the intake-gap signal: it closes the open segment and marks the next one `EXT-X-DISCONTINUITY`. On an empty buffer it emits nothing and still marks; repeating it is idempotent. `flush` closes the open segment as-is and is how the stream's tail is emitted.

`Segment` is `#[non_exhaustive]` and only `Segmenter` constructs one, so `mark_drop` is the sole path to a discontinuous segment.

## Playlist window

`LiveWindow` is the only mutator of window state. It keeps `window + grace` segments and lists the last `window` of them, so a segment evicted from the playlist stays fetchable for `grace` further segments and then leaves the retention. Cleanup follows the window; clients hold no state here.

`snapshot` returns a value: the playlist text, the retained segments, and whether the stream ended. Both payloads sit behind an `Arc`, so a snapshot is cheap to clone and hand to concurrent readers. `finished` is the authority on end-of-stream; `EXT-X-ENDLIST` is how the playlist renders it, and a reader answers the question from the flag.

The rendered playlist is version 3. `EXT-X-TARGETDURATION` is the configured segment target rounded up to whole seconds and nothing else: a client is told one value for the life of the stream, whatever the window holds. Rotation overshoots the target by at most one access unit, so every `EXTINF` still rounds to within that value, as RFC 8216 §4.3.3.1 requires. `EXT-X-MEDIA-SEQUENCE` is the first listed segment's sequence number and `EXT-X-DISCONTINUITY-SEQUENCE` follows it on every playlist, counting the discontinuous segments that have left the listed window; a client reloading after the window slid past a discontinuity can place it. Each segment carries `EXTINF` with three decimals and the URI `seg/<seq>.aac`, and a discontinuous segment is preceded by `EXT-X-DISCONTINUITY`. `finish` appends `EXT-X-ENDLIST`, which turns the tail into a valid VOD playlist; the owner stops pushing at that point.

## Service lifecycle

`Broadcast::start` consumes one complete `BroadcastConfig` and binds the origin before it returns, so the URL on the handle is one a client can already reach. It registers one `BroadcastTask` dispatcher on the configured shared `kithara-worker::Worker`; the HTTP origin retains its separate current-thread runtime thread.

The serving thread is a plain OS thread. `kithara-platform`'s spawns enrol a thread as a dedicated virtual-time pacer, while a tokio runtime parks in the OS event loop; enrolling that origin thread would freeze virtual-time quiescence. The cost is that a leak detector counting named platform threads does not see it, so origin shutdown is judged on its socket.

`BroadcastTask` is the sole mutator of the encoder, segmenter, and window. It publishes each closed segment by swapping a whole `PlaylistSnapshot` into an `ArcSwap`, so no request ever waits on packaging. The handle's completion slot is the crate's only mutex and the serving path never touches it; status counters belong to the task and the origin does not read them.

Nothing in media rotation reads a wall clock: the playlist changes only when encoded media closes a segment. Dispatcher wait and fairness settings affect work scheduling, not output timestamps.

`stop` atomically rejects later RT writes, drains the bounded PCM already handed over, finishes the encoder, flushes the tail segment, and publishes `EXT-X-ENDLIST`. The origin keeps serving that VOD tail. The first caller takes the completion slot and waits up to `stop_timeout`; repeated calls return immediately. Dropping `BroadcastOutput` requests the same graceful finish.

The PCM ring bounds the drain. On timeout the task and origin are cancelled rather than blocking application shutdown indefinitely.

Cancelling the token is the other axis: it stops the origin and task without promising a tail, including a drain already under way. `CancelScope::new(parent)` derives the broadcast subtree. Dropping `BroadcastHandle` cancels its task, dispatcher, and origin, so lifecycle ownership cannot leak when explicit stop is skipped.

## What the origin serves

`GET /master.m3u8` always answers: one `EXT-X-STREAM-INF` with `CODECS="mp4a.40.2"`, a `BANDWIDTH` of the audio bit rate plus the ADTS framing margin, and the URI `v/0/live.m3u8`. The `/v/0/` prefix reserves the variant slot for a later ladder.

`GET /v/0/live.m3u8` answers 404 until the first segment exists: an empty playlist is not a stream a client can start on. Playlists carry `application/vnd.apple.mpegurl` and `Cache-Control: no-store`; segments carry `audio/aac`. A segment past the retention is 404. CORS is permissive so a browser player can join.

## Intake

`BroadcastOutput` implements `kithara-output::LiveOutput` and accepts planar stereo master blocks. Its RT call performs only bounded ring writes, atomic accounting, and a deferred worker wake: no allocation, lock, encoding, filesystem, or network operation occurs there.

`buffer_frames` bounds the SPSC ring and `tick_frames` bounds one dispatcher pass. Samples that do not fit are counted monotonically and never block playback. The task observes each new drop debt once, closes the open segment, and marks the next segment discontinuous; it never inserts synthetic silence.

The app installs `BroadcastOutput` in the same `OutputGroup` protocol used by other master outputs. Broadcast no longer owns a parallel public feed or app-specific ring implementation.

## Time base

`Segment::duration_ts` is a `u32` in timescale units, so a segment tops out around 24 hours at 48 kHz; the segmenter errors rather than wrapping when an open segment outgrows it.
