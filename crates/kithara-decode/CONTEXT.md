# kithara-decode — Context

Contracts that no single file in this crate carries. The README is the overview;
repo-wide rules belong to [`AGENTS.md`](../../AGENTS.md). Owned elsewhere and
never re-derived here: encoded/container media types and
`AudioCodec::encoder_priming_frames` by
[`kithara-stream`](../kithara-stream/CONTEXT.md); decoded-signal values and pure
sample/time math by [`kithara-signal`](../kithara-signal/CONTEXT.md); resampler
traits, config, and backend families by
[`kithara-resampler`](../kithara-resampler/CONTEXT.md); the MPEG-audio packet
transaction by [`kithara-mpa`](../kithara-mpa/CONTEXT.md).

## Backend selection

`DecoderConfig::backend` picks exactly one backend; `MediaInfo` supplies
codec/container, never the backend. A `DecoderBackend` variant exists only when
its feature and `target_os` are active, so an impossible pick is a compile error,
not a runtime one. No fallback chain: a backend that rejects the codec/container
returns terminal `DecodeError::UnsupportedCodec`. With `symphonia` off there is
no software fallback at all — the compiled hardware backend is the whole surface.

- `create_from_media_info` (recreate / HLS) **never runs Symphonia's probe**.
  Mid-segment bytes at a mismatched offset can silently match an unrelated codec.
  It builds a `ProbeHint` from `MediaInfo` and propagates failures verbatim.
- `create_with_probe` is the extension-hint path. MP4/M4A is container-only
  (AAC/ALAC/FLAC all live there), so it sniffs the `stsd` sample-entry fourcc. A
  missing container falls to `sniff_container_from_source`: a bounded 12-byte
  prefix byte sniff that rewinds to 0 on every exit path — never a Symphonia probe.
- `SymphoniaDemuxer::open_file` constructs with `seek_enabled` false and
  re-enables afterwards, because `IsoMp4Reader` / `WavReader` would otherwise
  stall an HLS source by seeking to the tail during construction. Standard `Mp4`
  is the sanctioned exception (`moov` sits at the tail); it is safe only because
  standard-MP4 consumers pass a fully materialised source.

## Reader profile

`DecoderFactory::reader_profile` returns the reading *discipline* of the demuxer
the factory would build. It is not a byte window: the byte-space owner —
`kithara-audio`'s readiness gate — resolves it to a range in its own coordinate
space, because only that layer knows the ABR virtual byte shift.

Under `ReaderInput::InitOnly` the init header (`moov` / `esds` / `STREAMINFO`)
must be buffered before construction, but the landing media segment must **not**
be. The first `next_frame` reads it and pends until it arrives, so gating the
build on it would be circular.

`should_use_segment_aware` and `reader_profile` share `segment_aware_container`,
so the build decision and the gate's readiness decision cannot disagree.

## ComposedDecoder

`ComposedDecoder<D, C, S>` is the single decode loop; every backend is a
`(demuxer, codec)` pair fed through it.

- **Frame offset is cumulative**, anchored to `landed_at` on seek. `frame_at`
  rounds half-up so it agrees with `frames_to_trim`; a floor would disagree by
  one frame at a mid-playback recreate.
- **Zero-frame budget** (`ZERO_FRAME_BUDGET`). Queue codecs absorb input without
  producing PCM, so the loop reports `SourcePending` rather than consuming the
  demuxer to EOF. Positive PCM output and every seek reset the counter.
- `timeline_gap_frames` is `max(head_strip, timestamp_bias_frames + observed
  forward PTS jumps)`. The maximum is what makes a head-start decode (strip split
  between modelled bias and observed jump) and a mid-stream decode (resyncs on
  seek, no jump) agree on the number a splice cuts on.

## Seek pre-roll and trim

Two steps, split across the demuxer and `ComposedDecoder`.

1. **Demuxer back-off.** `Demuxer::seek(target, priming)` parks the cursor before
   the target. `CodecPriming` covers two distinct needs: MDCT/SBR/PS warm-up
   (pre-target packets must be decoded so overlap-add and QMF state converge) and
   at least one whole codec packet, so the trim lands on a packet boundary. The
   second is derived from `access_unit_frames` and the track `sample_rate` — no
   magic millisecond constants. Backends that prime internally keep
   `CodecPriming::default()`; `AppleCodec` carries the only per-codec table, and
   its values are pinned in `apple/codec.rs`. HE-AAC v1/v2 are seen as `AacLc` by
   both the fMP4 init parse and the codec layer; fdk-aac auto-detects SBR.

   For `Fmp4` the back-off may cross a segment boundary. `Fmp4SegmentDemuxer` then
   decode-and-discards the tail of the prior segment so SBR converges across it,
   and reads stay confined to the pre-roll segment plus the target segment —
   never a prefix walk from segment 0. `byte_margin` drives the `PrerollHint`.
2. **Sample-accurate trim.** `pending_seek_target` drops whole pre-target frames,
   then trims leading samples of the straddling frame so the emitted chunk starts
   exactly at the target rather than at the packet boundary. Queue codecs hold the
   pending target across zero-frame decode calls and report
   `FrameCodec::decoded_pts`, so the trim is evaluated against decoded output, not
   the packet fed in. Without it a seek leaks up to one packet of pre-target audio.

### `landed_byte` on the Apple path — sanctioned degraded mode

`DemuxSeekOutcome::Landed`'s `landed_byte` realigns the stream's byte cursor with
where the decoder resumes (`kithara-audio` calls `stream.set_position`).
Reporting `None` leaves the cursor at the pre-seek offset, and a reopened track
then mis-classifies the post-seek read as EOF.

`AppleAudioFileDemuxer` answers it from `kAudioFilePropertyPacketToByte` when the
open knows the total size, and otherwise from a linear estimate scaled off the
live byte-length handle. The estimate is not papering over a state bug: measured,
a size-less open rejects *every* packet with
`kAudioFileInvalidPacketOffsetError`, and the offset Apple would seek to is
exposed nowhere else at seek time. Approximating is safe because the value drives
the byte-oriented stream's own cursor and progress events, never the decoder's
reads, which `AudioFileServices` issues at absolute offsets through its callbacks.

The two branches cannot leave a gap, because one flag selects both the open mode
and the mechanism: `factory::inner` opens streaming exactly when
`config.byte_len_handle` is `Some`. A size-less open therefore always has the
handle the estimate needs, and a sized open always has the mapping Apple answers
from. Both are pinned in the two combinations production uses —
`size_less_mp3_seek_reports_landed_byte_from_the_length_handle`, and
`sized_mp3_seek_reports_landed_byte_without_a_length_handle`, which attaches no
handle so only Apple's mapping can answer.

## What ends a segmented stream

`ByteMap::segment_at_index` answers about the layout as published so far, so
`None` means "this index is outside the current layout" - which covers both a
segment past the last one and a segment the layout has not described yet.
`ByteMap::segment_count` is what separates them: an index the layout counts
names a segment that exists and is still owed.

`Fmp4SegmentDemuxer` therefore ends the stream only past that count, and parks
on `PendingReason::Retry` inside it. Reading an
undescribed index as the end is not a slow path, it is a wrong answer: the
incoming generation of an ABR up-switch reports EOF before it has staged a
single frame, the transition is discarded with `abort_intent`, and the player
never switches. `an_undescribed_segment_is_not_the_end_of_the_stream` pins it.

## Read-ahead strand

Over an HLS `Stream`, `next_frame` can be interrupted at a not-yet-downloaded
segment boundary. Symphonia's `MediaSourceStream` consumes bytes from its
read-ahead ring *before* it knows the read can complete; on `Interrupted` the
half-read packet is discarded but the position stays advanced. Those bytes are
**stranded**, and byte-position-quantised readers (WAV/PCM, packet pts derived
from stream position) then silently skip them.

`MediaSourceStream` exposes no per-call rewind through `FormatReader`, so
`SymphoniaDemuxer` makes the **decoder's timestamp authoritative across a
pending**: it tracks `resume_ts`, and `reseek_to_resume` restores it on the next
call, staying armed if the recovery itself pends. The re-seek is a bare position
restore — no pre-roll back-off, no codec flush — and stays in native timebase
units, because a `Duration` round-trip loses packet-boundary precision. An
accurate seek may land one packet early, so recovery discards packets ending at
or before `resume_ts`.

Native MPEG audio (`FORMAT_ID_MP1` / `FORMAT_ID_MP2` / `FORMAT_ID_MP3`) is
exempt. [`kithara-mpa`](../kithara-mpa/CONTEXT.md) — a fork of Symphonia's
`MpaReader` that `registry::get_probe` registers in place of the upstream one —
owns a byte-exact packet transaction, and layering timestamp recovery over it
would double-correct. Seek transactionality remains a separate, unsolved concern:
pending from inside `MpaReader::seek` is not made resumable by that contract. The
strand never reaches the `Stream` / `wait_range` contract.

## Gapless playback

`leading_frames` / `trailing_frames` in `GaplessInfo` are always
**decoder-output** PCM frames — the trimmer-input domain. Under Apple fused
decode+SRC the factory scales source-rate container metadata once before codec
open (`track_with_output_domain_gapless`), so `GaplessTrimmer` never sees mixed
domains.

One owner for actual trimming. `Some` gapless on `DecoderTrackInfo` means the
backend decoded the untrimmed PCM region and the `kithara-audio` pipeline must
apply `GaplessTrimmer` before effects. `None` means no engine trim — either no
metadata, or a backend path that already trimmed internally.

`GaplessTrimmer::notify_seek` retires buffered chunks through a `ChunkRetire`
rather than dropping them: returning a pooled `AudioChunk` to a full shard
deallocates, and the caller is the produce core. `DropChunks` is the sink for
callers that are free to deallocate.

`GaplessTailCompensation` is tail-side and track-local — no measured deficit ever
flows into the next track's leading trim. Both the fused Apple path and the
standalone `ResampledDecoder` publish it. `AppleCodec::SRC_OUTPUT_MARGIN_FRAMES`
is the matching ceil-domain slack in the converter's output sizing: a correctness
constant, not a tunable, local to the Apple codec owner.

Heuristic fallbacks are selected by `GaplessMode` on `kithara-audio`'s
`AudioConfig`, and every heuristic trim applies a short raised-cosine fade at the
boundary. The metadata-driven path does not, because that boundary is sample-exact.

### Two independent silence layers

Encoder-side priming/padding and decoder-side algorithmic delay are separate, and
both must be accounted for.

- `probe_codec_gapless` returns `Some` only when real container metadata exists
  (MP4 `elst` or `iTunSMPB`, Xing/Info + LAME). **No fallback chain**: absent
  metadata returns `None` and the pipeline falls through to
  `AudioCodec::encoder_priming_frames`. `scoped_probe` / `scoped_startup_probe`
  rewind to 0 before and after, surfacing a failed rewind as an error instead of
  letting the demuxer start mid-file.
- The MP3 probe window is fixed and measured from the first audio byte. An
  `ID3v2` tag declares its own length, so the probe skips it rather than widening
  the window; a short read means the source ran out of ready bytes, not a long
  tag, and the probe stays put.
- `FrameCodec::decoder_algo_delay` carries the decoder half. Upstream trap: the
  Symphonia `mpa` demuxer parses the LAME tag into `track.delay`, but the
  0.6.0-alpha demuxer does not populate per-packet `trim_start` / `trim_end`, so
  `opts.gapless` is a no-op for MP3 and the caller must apply the trim. Android
  `MediaCodec` surfaces no priming at all.
- Measured: raw Symphonia output of a libmp3lame sawtooth (`enc_delay` 576)
  starts at sample 1105, raw Apple output at 576. **Both backends ignore the LAME
  tag; only the probe reads it.** `SymphoniaCodec::open_with_config` folds its own
  algo delay into the probed `GaplessInfo` so the audio pipeline reads one
  fully-resolved trim, and `Decoder::default_priming_frames` exposes the same
  combined number so `kithara_audio::pipeline::gapless` need not know the backend.

## Resampler placement

`DecoderConfig::resampler` selects between two placements. Codec-embedded: a
backend that already owns a converter emits target-rate PCM directly (Apple), and
`embedded_target_output_rate` decides whether the fused path is taken.
Standalone: `resampled::wrap` wraps the decoder and owns interleaving, pooled
planar scratch, target-rate metadata, and seek/gapless domain scaling — skipped
when the decoder already emits the target rate. Invalid backend/config pairs fail
at construction; nothing tries another backend.

Shared `AudioToolbox` FFI (`AudioConverter`, `AudioFile`, `AudioBufferList`, POD
byte-copy wrappers) stays in `kithara-apple`; the standalone PCM-to-PCM Apple
backend stays in `kithara-resampler`. This crate owns codec planning, gapless
policy, and the codec-embedded Apple decode path.

`sanitize_sample` is the workspace's one sample guard — `NaN`, infinities and
denormals become silence. `ResampledDecoder::append_chunk` applies it while
deinterleaving, so a backend only ever sees finite normal input: the adapter is
the last owner of the samples by value, while `Resampler::process_into_buffer`
takes them by shared reference. `kithara-audio` and `kithara-play` reuse the
function at their own untrusted-input stages. Pinned by
`resampler_never_sees_a_sample_the_file_poisoned`.

## Apple AAC and FLAC cookies

Demuxers hand the Apple codec either a raw `AudioSpecificConfig` body (fMP4 /
HLS) or a full `ES_Descriptor` body (`AppleAudioFileDemuxer` reads
`kAudioFilePropertyMagicCookieData`, already complete for M4A). A single-byte
sniff disambiguates the two without parsing, and `esds_wrap_asc` builds the
minimum ISO/IEC 14496-1 descriptor chain Apple accepts for the raw case;
`build_aac_input_format` documents why manual ASBD construction fails on HE-AAC.

After cookie installation Apple's `FormatList` ASBD — not the demuxer's
`TrackInfo` — is authoritative for sample rate, channel count, and frames per
packet: HE-AAC v2 doubles the rate versus the container declaration. FLAC mirrors
the two-carrier problem, and `apple::flac::streaminfo_body` normalises both the
raw fMP4 `dfLa` payload and the full `FLACSpecificBox` cookie to one body.

## WebCodecs host worker

**A worker spawned by a blocked parent worker does not execute** — true for a
parent blocked by spinning and for one blocked by `Atomics.wait`. The play-owned
producer scheduler is such a parked parent, so neither the codec nor the decode
path may spawn the host. Only the main-thread bootstrap `spawn_webcodecs_probe`,
whose event loop stays live, may. It creates one host worker and probes the
compile-time codec configurations through `AudioDecoder::is_config_supported`.
The immutable host sender and the completed support table publish through
separate `OnceLock`s; until the snapshot lands the backend support check returns
`false`, so the synchronous factory takes the Symphonia path. Opening a WebCodecs
codec before runtime initialization is a typed contract error.

The host command loop uses `try_recv` plus a local timer; the synchronous
per-decoder reply receiver uses the Atomics-backed `recv_timeout` path and needs
no event loop. The singleton host receives the app's typed `PoolRegion` at spawn.
`HostOut::Pcm` carries a pooled `SampleBuffer` that the codec moves into the
caller's output buffer — JavaScript values never cross the Rust thread boundary.

The frame codec owns the current generation. A seek advances it, resets, discards
queued output, then **reconfigures**, because WebCodecs `reset()` returns
`AudioDecoder` to the unconfigured state; commands and callbacks from older
generations are dropped everywhere. `WebCodecsCodec::needs_eof_drain` is
unconditionally true — the browser pipeline queues output at every rate. The
first empty frame flushes, and each drain call waits within one bounded budget
for current-generation PCM, an explicit flush completion, or an error. Budget
exhaustion is a typed backend error, never a synthetic EOF.

Capability probing needs a synthetic STREAMINFO, kept in sync between
`webcodecs/probe.rs` and `tests/webcodecs_browser.rs`. The HE-AAC v1 and v2
browser bodies are generated at build time by `kithara-test-fixtures`
(`he_aac_v1` / `he_aac_v2`) and embedded, because wasm has no fixture store to
read them from. The browser lane is advisory in CI and needs a working
ChromeDriver:

```bash
just test wasm chrome webcodecs
```

## Tests

Cross-backend tests live outside this crate, in `kithara-integration-tests` under
`tests/tests/kithara_decode/`. `protocol_tests.rs` decodes the same MP3 with
every available backend and asserts they agree on spec, duration, total frames,
post-seek timestamp, EOF semantics, and — when `apple` is enabled on macOS/iOS —
full-decode PCM L2 norm.
