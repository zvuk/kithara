# kithara-broadcast — Context

Contracts and invariants for the kithara-broadcast crate; the README is the overview.

The crate is a pure packaging core: it turns `kithara-encode` access units into HLS media segments and playlist text. It performs no I/O, spawns no threads, and holds no clock — the caller drives it.

## ADTS framing

Every access unit gets a 7-byte ADTS header (no CRC): MPEG-4, layer 0, `protection_absent`, AAC-LC profile, `buffer_fullness = 0x7FF` (VBR), one raw data block. Each frame is a sync point, so segments are byte-concatenatable and a client joining at any segment decodes it standalone.

The header's `sampling_frequency_index` and `channel_configuration` fields are fixed at construction, so audio ADTS cannot describe is rejected there: a sample rate outside the 13-entry table (96000 … 7350 Hz) and a channel count outside 1..=7 both error. `frame_length` is 13 bits, capping one access unit at 8184 payload bytes.

## Segment rotation

`Segmenter` runs on the media clock: it sums the `duration` of the access units it framed, in the timescale those durations are expressed in. `kithara-encode` guarantees the durations tile the pts timeline, so the sum is the segment's exact media length.

Rotation is append-then-check: the access unit whose duration carries the accumulated total to the target belongs to the segment it closes. A segment therefore runs slightly past the target rather than short of it, and segment durations sum to the pushed audio exactly.

Sequence numbers count emitted segments from zero. An empty buffer emits nothing, so a close with no framed audio consumes no sequence number.

`mark_drop` is the intake-gap signal: it closes the open segment and marks the next one `EXT-X-DISCONTINUITY`. On an empty buffer it emits nothing and still marks; repeating it is idempotent. `flush` closes the open segment as-is and is how the stream's tail is emitted.

Its caller is the serving layer, which reports the gap — so the `dead_exports` ratchet reports `mark_drop` as an export with test-only references, and that report stands unsuppressed. `Segment` is `#[non_exhaustive]` and only `Segmenter` constructs one, so `mark_drop` is the sole path to a discontinuous segment: dropping it would make `Segment::discontinuity` and the playlist's `EXT-X-DISCONTINUITY` branch unreachable.

## Playlist window

`LiveWindow` is the only mutator of window state. It keeps `window + grace` segments and lists the last `window` of them, so a segment evicted from the playlist stays fetchable for `grace` further segments and then leaves the retention. Cleanup follows the window; clients hold no state here.

`snapshot` returns a value: the playlist text, the retained segments, and whether the stream ended. Both payloads sit behind an `Arc`, so a snapshot is cheap to clone and hand to concurrent readers.

The rendered playlist is version 3: `EXT-X-TARGETDURATION` is the longest listed `EXTINF` rounded up to whole seconds, `EXT-X-MEDIA-SEQUENCE` is the first listed segment's sequence number, each segment carries `EXTINF` with three decimals and the URI `seg/<seq>.aac`, and a discontinuous segment is preceded by `EXT-X-DISCONTINUITY`. `finish` appends `EXT-X-ENDLIST`, which turns the tail into a valid VOD playlist; the owner stops pushing at that point.

An empty window renders `EXT-X-TARGETDURATION:0` and `EXT-X-MEDIA-SEQUENCE:0`. What an origin serves before the first segment exists is the serving layer's contract, not this crate's.

`Segmenter` and `LiveWindow` are given the timescale separately and must be given the same one — the encoder's — for `EXTINF` to describe the segments the segmenter cut.

## Time base

`Segment::duration_ts` is a `u32` in timescale units, so a segment tops out around 24 hours at 48 kHz; the segmenter errors rather than wrapping when an open segment outgrows it. Nothing here reads a wall clock: rotation is media-driven and a playlist changes only when a segment closes.
