# kithara-broadcast — Context

Contracts and invariants for the kithara-broadcast crate; the README is the overview.

The crate turns `kithara-encode` access units into HLS media segments and serves them: a packaging core with no clock of its own, and a service that drives it from a PCM feed and publishes it over HTTP.

## Configuration

`BroadcastConfig` is the single knob surface. `Segmenter`, `LiveWindow`, and `Broadcast::start` all take it, so the sample rate is the media timescale everywhere and no caller can pair a segmenter with a window that disagrees about time.

`validate` rejects zero audio, a zero window, a segment target shorter than one media tick, and — per RFC 8216 §6.2.2 — a window that spans fewer than three target durations. A short window is a typed `PlaylistTooShort`, never a silent adjustment.

## ADTS framing

Every access unit gets a 7-byte ADTS header (no CRC): MPEG-4, layer 0, `protection_absent`, AAC-LC profile, `buffer_fullness = 0x7FF` (VBR), one raw data block. Each frame is a sync point, so segments are byte-concatenatable and a client joining at any segment decodes it standalone.

The header's `sampling_frequency_index` and `channel_configuration` fields are fixed at construction, so audio ADTS cannot describe is rejected there: a sample rate outside the 13-entry table (96000 … 7350 Hz) errors, and so does a channel count outside 1..=6 — `channel_configuration` counts channels only up to 6, and its remaining value stands for 7.1, which is a layout rather than a count.

`kithara-decode` builds the same header shape for its fdk-aac transport (`symphonia/aac_fdk.rs`). The two stay separate: neither crate depends on the other, and a shared owner for seven bytes would couple the decode path to the broadcast path.

## Packed-audio timestamp

RFC 8216 §3.4 requires every packed-audio segment to open with an ID3v2 tag whose single PRIV frame is owned by `com.apple.streaming.transportStreamTimestamp` and whose 8-octet big-endian body is the first sample's MPEG-2 timestamp. `TimestampTag` renders that fixed-size tag and `Segmenter` prepends it when it closes a segment, from the stream time the segment started at.

`TimestampTag::mpeg_timestamp` is the only place the media timescale and the 90 kHz MPEG-2 domain meet: it rounds to the nearest 90 kHz tick and wraps at 33 bits. Everything else in the crate counts in the configured sample rate.

The stream clock counts encoded audio only. `mark_drop` closes a segment and marks the next one discontinuous; it does not synthesise a gap in the timestamps, because `EXT-X-DISCONTINUITY` is what tells a client the timeline broke.

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

The rendered playlist is version 3. `EXT-X-TARGETDURATION` is the configured segment target rounded up to whole seconds, raised only if a listed segment is longer — a client is told one value for the life of the stream, and a window of drop-truncated segments cannot lower it. `EXT-X-MEDIA-SEQUENCE` is the first listed segment's sequence number and `EXT-X-DISCONTINUITY-SEQUENCE` follows it on every playlist, counting the discontinuous segments that have left the listed window; a client reloading after the window slid past a discontinuity can place it. Each segment carries `EXTINF` with three decimals and the URI `seg/<seq>.aac`, and a discontinuous segment is preceded by `EXT-X-DISCONTINUITY`. `finish` appends `EXT-X-ENDLIST`, which turns the tail into a valid VOD playlist; the owner stops pushing at that point.

## Service lifecycle

`Broadcast::start` binds the origin before it returns, so the URL on the handle is one a client can already reach. Two threads run behind it: a worker that polls the feed and packages it, and a thread with a current-thread runtime that serves axum.

The worker is the sole mutator of the segmenter and the window. It publishes each closed segment by swapping a whole `PlaylistSnapshot` into an `ArcSwap`, so a request never waits on the worker and the two threads share no lock. The handle's join slot is the crate's only mutex and the serving path never touches it.

Nothing in the pipeline reads a wall clock: rotation is media-driven and the playlist changes only when a segment closes. The worker's poll backoff paces an empty feed and is not part of the contract.

`stop` ends the broadcast: the worker swallows what the feed still holds, drains the encoder, flushes the tail segment, and publishes the playlist with `EXT-X-ENDLIST`. The origin keeps serving that VOD tail, and repeat calls do nothing. A feed that reports its producer gone — the app disabling the tap, a device rate change, session teardown — takes the same graceful path and leaves the stream off air.

Cancelling the token is the other axis: it stops the origin and the worker without a tail. `CancelScope::new(parent)` derives the broadcast's subtree, so the app cancels the broadcast by cancelling what it owns. Dropping the handle is passive — it cancels nothing.

## What the origin serves

`GET /master.m3u8` always answers: one `EXT-X-STREAM-INF` with `CODECS="mp4a.40.2"`, a `BANDWIDTH` of the audio bit rate plus the ADTS framing margin, and the URI `v/0/live.m3u8`. The `/v/0/` prefix reserves the variant slot for a later ladder.

`GET /v/0/live.m3u8` answers 404 until the first segment exists: an empty playlist is not a stream a client can start on. Playlists carry `application/vnd.apple.mpegurl` and `Cache-Control: no-store`; segments carry `audio/aac`. A segment past the retention is 404. CORS is permissive so a browser player can join.

## Intake

`LivePcmFeed` is the crate's PCM seam: one `poll` appends interleaved f32 and reports the gap that preceded it plus end-of-stream, so the worker cannot see the producer leave while samples are still pending. `FeedChunk` is deliberately not `#[non_exhaustive]` — implementors outside the crate construct it. A non-zero `dropped` closes the open segment and marks the next one discontinuous; there is no backpressure on the audio path and no silence injection.

## Time base

`Segment::duration_ts` is a `u32` in timescale units, so a segment tops out around 24 hours at 48 kHz; the segmenter errors rather than wrapping when an open segment outgrows it.
