# kithara-decode — Context

Contracts and invariants for `kithara-decode`; the README is the overview.

## Backend selection and initialization

`DecoderConfig::backend` (a `DecoderBackend`) picks exactly one backend;
`MediaInfo` supplies codec/container, never the backend. A variant exists in the
type only when its feature and `target_os` are active, so an impossible pick is a
compile error, not a runtime `BackendUnavailable`. No fallback chain: a backend
that rejects the codec/container returns terminal `DecodeError::UnsupportedCodec`.

- `create_from_media_info` — recreate / HLS path; builds a `ProbeHint` from
  `MediaInfo`, failures propagate verbatim. **Never runs Symphonia's probe** —
  mid-segment bytes at a mismatched offset can silently match an unrelated codec.
- `create_with_probe` — extension-hint path; MP4/M4A is container-only
  (AAC/ALAC/FLAC all live there), so it sniffs the `stsd` sample-entry fourcc.
- `dispatch_backend` fills a missing container by a bounded 12-byte prefix byte
  sniff (`sniff_container_from_source`), rewinding to 0 on every exit path — a
  byte sniff, not a Symphonia probe.
- `new_direct(container)` builds the concrete Symphonia `FormatReader` with no
  probe and **disables seek during construction** (so `IsoMp4Reader` / `WavReader`
  cannot stall an HLS source by seeking to the tail), re-enabling seek
  unconditionally afterwards. Standard `Mp4` is the exception — `moov` sits at the
  tail, so it constructs seek-enabled over a materialised source.
  `probe_with_seek(..., seek_enabled)` is auto-detection; `open_file` passes
  `seek_enabled = false`, then re-enables.

## Reader profile contract

`Demuxer::required_input() -> kithara_stream::ReaderInput` declares the *shape*
of input a demuxer needs before construction (default `Incremental`;
`Fmp4SegmentDemuxer` overrides to `InitOnly`). It is a reading *discipline*, not a
byte window — the concrete init range is resolved by the byte-space owner (the
stream layer), which alone knows the ABR virtual byte shift.

- `InitOnly` — the init header (moov/esds/STREAMINFO) must be buffered before
  construction. The landing media segment is read later by the first `next_frame`
  and pends until it arrives, so it is *not* a build prerequisite (gating on it
  would make build-then-pend circular).
- `Incremental` — self-framing input (MP3/FLAC/Ogg, Apple `AudioFile`, Android
  `MediaExtractor`): nothing gated up front.

`DecoderFactory::reader_profile(media_info, byte_map) -> ReaderProfile` bridges to
the kithara-audio readiness gate: it returns the contract of the demuxer the
factory *would* build, and the gate resolves it to bytes in its own coordinate
space. Segment-aware container **and** byte map present → `InitOnly`;
`ContainerFormat::Wav` → `InitOnly` (header not self-framing); else `Incremental`.
The profile also carries `ReaderWarmup::None` and a 32 KiB read-ahead.
Segment-aware means `!kithara_stream::needs_exact_byte_sizes(codec, container)` —
AAC (LC/HE/HEv2) or FLAC in `Fmp4` — plus `DecoderConfig::byte_map.is_some()`;
`should_use_segment_aware` and `reader_profile` share that predicate so they
cannot disagree.

## ComposedDecoder invariants

`ComposedDecoder<D: Demuxer, C: FrameCodec>` is the single decode loop; every
backend is a `(demuxer, codec)` pair fed through it. `DecoderRuntime` carries the
PCM pool, epoch, byte-length handle, and reader hooks; `pool` has no `Default` —
the host threads its configured pool down, and test-only
`DecoderRuntime::for_test` (`#[cfg(test)]`) is the only place the global pool is
reachable.

- **Frame offset is cumulative**, anchored to `landed_at` on seek and advanced by
  each emitted chunk (per-chunk `floor(pts * rate)` loses precision).
  `frame_offset_for` rounds half-up to agree with `frames_to_trim`; a floor would
  disagree by one frame at a mid-playback recreate.
- **Zero-frame budget.** After 32 consecutive zero-frame `decode_frame` calls the
  decoder returns `Pending(NotReady(SourcePending))` instead of consuming the
  demuxer to EOF (queue codecs absorb input without producing PCM); any positive
  PCM output and every seek reset the counter.
- **EOF drain** is owned by `FrameCodec::needs_eof_drain(source_sample_rate)`,
  defaulting to "output rate differs from source rate" so fused-SRC backends keep
  their contract. On demux EOF the decoder feeds empty frames until the codec
  returns zero.
- **Head strip.** `HeadStrip` counts frames dropped from the head as they are
  dropped, measured against packet duration converted at the *output* rate (SBR:
  a 1024-core-frame AU with 2048 output frames is measured against 2048). It
  settles the first time a packet comes back whole; a short packet after that is
  the stream's own tail.
- `Decoder::timeline_gap_frames` = `max(head_strip, codec.timestamp_bias_frames() + observed forward PTS jumps)`; the maximum leaves
  a head-start decode (strip split between modelled bias and observed jump) and a
  mid-stream decode (resyncs on seek, no jump) with the number a splice cuts on.
  It is **live**: caching it at construction holds the pre-strip value forever.
- `Decoder::default_priming_frames` = `AudioCodec::encoder_priming_frames` plus
  the codec's `decoder_algo_delay` (delegated).
- Reader hook events resolve on the forbid-blocking decode core and publish later
  through `Decoder::flush_reader_signals`, drained once per pass by the audio
  worker's unchecked shell.

## Seek pre-roll and trim

Two-step contract shared by the demuxer and `ComposedDecoder`.

1. **Demuxer back-off (pre-roll).** `Demuxer::seek(target, priming)` parks the
  cursor *before* the target; magnitude comes from `FrameCodec::priming(codec) -> CodecPriming { packets, frames, byte_margin }`, covering two needs.

- **Codec warm-up** — MDCT/SBR/PS codecs must decode pre-target packets to
  converge overlap-add and QMF state. `SymphoniaCodec` requests 2 packets for
  AAC (HE-AAC v1/v2 are seen as `AacLc` by both the fMP4 init parse and the
  codec layer; fdk-aac auto-detects SBR). `AppleCodec` per-codec table:
  HE-AACv2 `frames 4096 / packets 3 / byte_margin 32768`, HE-AAC
  `2048 / 2 / 16384`, AAC-LC `1024 / 2 / 8192`, MP3 `1152 / 1 / 4608`; codecs
  that converge instantly keep `CodecPriming::default()`.
- **At least one whole codec packet**, so the trim lands on a packet boundary.
  Derived from codec facts (`access_unit_frames`: 1024 AAC, 1152 MP3) and the
  track `sample_rate` — no magic millisecond constants.
  For fMP4 the back-off can cross a segment boundary; `Fmp4SegmentDemuxer::seek`
  then decode-and-discards the tail of the prior segment so SBR converges across
  it. Reads stay confined to the pre-roll segment plus the target segment — never
  a prefix walk from seg-0. `byte_margin` drives the returned `PrerollHint`.

1. **Sample-accurate trim.** `ComposedDecoder::pending_seek_target` drops whole
  pre-target frames, then trims leading samples of the straddling frame
  (`frames_to_trim`, round-to-nearest) so the emitted chunk starts exactly at
  `target`, not at the packet boundary. Queue codecs retain the pending target
  across zero-frame decode calls and report the timestamp of the PCM that
  eventually surfaces (`FrameCodec::decoded_pts`), so the trim is evaluated
  against decoded output, not the packet fed. Without it a seek leaks up to one
  packet of pre-target audio.

## Read-ahead strand

Over an HLS `Stream`, `next_frame` can be interrupted at a not-yet-downloaded
segment boundary. Symphonia's `MediaSourceStream` (MSS) consumes bytes from its
read-ahead ring *before* it knows the read can complete; on `Interrupted` the
half-read packet is discarded but MSS's position stays advanced — those bytes are
**stranded**, and byte-position-quantised readers (WAV/PCM, packet pts derived
from stream position) then silently skip them.

MSS exposes no per-call rewind through `FormatReader`, so `SymphoniaDemuxer` makes
the **decoder's timestamp authoritative across a `Pending`**: it tracks
`resume_ts` (native timebase units — `actual_ts` on seek, `pts + dur` after each
emitted packet) and sets `needs_resume` on any interrupted read; the next
`next_frame` re-seeks to `resume_ts` first so the interrupted packet is re-read
from its start. That re-seek is a bare position restore — no pre-roll back-off, no
codec flush (that is the user-seek path) — and is idempotent when no strand
occurred. `resume_ts` stays native: a `Duration` round-trip loses sub-frame
precision and snaps the re-seek one packet early. The strand never reaches the
`Stream` / `wait_range` contract.

## Gapless playback

`DecoderConfig::gapless` defaults to true. Decoders report engine-level trim
through `DecoderTrackInfo { gapless: Option<GaplessInfo>, gapless_tail: Option<GaplessTailCompensation> }`. `leading_frames` / `trailing_frames` are
always **decoder-output** PCM frames — the trimmer-input domain. Under Apple
fused decode+SRC the factory scales source-rate container metadata once before
codec open (`track_with_output_domain_gapless`, round-half-up) so
`GaplessTrimmer` never sees mixed domains. One owner for actual trimming:
`Some(GaplessInfo)` means the backend decoded the untrimmed PCM region and the
`kithara-audio` pipeline must apply `GaplessTrimmer` before effects; `None` means
no engine trim (no metadata, or a backend path that already trimmed internally).
`GaplessTrimmer::notify_seek(retire)` drops seek-sensitive state (leading trim,
pending fade-in, buffered tail, tail compensation); trailing trim still applies
at EOF. It takes a `ChunkSink` rather than dropping the buffered chunks itself:
a `PcmChunk` holds a pooled buffer, returning one to a full pool shard
deallocates, and the caller is the produce core. `DropChunks` is the sink for
callers that are free to deallocate.

`Decoder::gapless_profile(codec) -> GaplessProfile` bundles spec, gapless, tail
compensation, and `default_priming_frames` for trimmer construction, referencing
the existing contracts rather than duplicating frame counts.

### Fused-SRC tail compensation

`AppleCodec` publishes `GaplessTailCompensation::for_source_frames(seen, source_rate, output_rate)` — the *ideal* pre-trim output length,
`ceil(source_frames * output_rate / source_rate)`. At EOF the trimmer reduces
fixed trailing trim by the deficit against the decoder-output frames actually
received, **bounded to at most one frame** (a larger deficit logs a warning and
still clamps). Compensation is tail-side and track-local: no measured deficit
flows to the next track's leading trim. `AppleCodec::SRC_OUTPUT_MARGIN_FRAMES = 1`
is the matching ceil-domain slack in the converter's output sizing — a correctness
constant, not a tunable, local to the Apple codec owner.

### Heuristic fallbacks

When metadata is absent, `kithara-audio`'s `AudioConfig::gapless_mode` picks
behaviour via `GaplessMode`: `Disabled` (passthrough, decoder `GaplessInfo`
ignored); `MediaOnly` (default — decoder counts when present, else nothing);
`CodecPriming` (`GaplessTrimmer::codec_priming` fed by
`Decoder::default_priming_frames`; predictable, zero-latency); or
`SilenceTrim(SilenceTrimParams)` (`GaplessTrimmer::silence_trim` walks the leading
buffer to the first sample above a dB threshold — default 45 dB below full scale,
`min_trim_frames` 256, `scan_window_frames` 4096 — and optionally trims trailing
silence at EOF over a 10 ms energy window, never per-sample). A non-positive or
non-finite threshold yields linear amplitude 1.0, disabling trim rather than
eating audible content.

Both heuristic paths apply a ~3 ms raised-cosine fade-in at the trim boundary (and
a matching fade-out after a heuristic trailing trim). The metadata-driven path
does not — that boundary is sample-exact.

### Gapless probe contract

Silence has two independent layers.

1. **Encoder-side priming / padding.** `probe_codec_gapless` reads container
  metadata and returns `Some` only when real values exist: MP4 `elst` /
  `iTunSMPB` for AAC (`probe_mp4_gapless`, `elst` wins), Xing/Info + LAME for
  MP3. **No fallback chains** — absent metadata returns `None` and the pipeline
  falls through `GaplessMode::CodecPriming` to
  `AudioCodec::encoder_priming_frames` (MP3 576, AAC 1024, Opus 312,
  FLAC/Vorbis/ALAC/PCM/ADPCM 0). `scoped_probe` / `scoped_startup_probe` wrap the
  probe with rewind-to-0 before and after, surfacing a failed rewind as an error
  instead of letting the demuxer start mid-file. The MP3 probe window is 16 KiB
  from the first audio byte: an `ID3v2` tag declares its own length, so the probe
  skips it rather than widening the window. A short read means the source ran out
  of ready bytes, not a long tag — the probe stays put then.
1. **Decoder-side algorithmic delay**, on `FrameCodec::decoder_algo_delay`:

- Symphonia `mpa` (LAME convention): +529 leading, −529 trailing for MP3 (528
  polyphase convergence + 1 sync sample). Its demuxer parses the LAME tag into
  `track.delay`, but the 0.6.0-alpha demuxer does not populate per-packet
  `trim_start` / `trim_end`, so `opts.gapless` is a no-op for MP3 — the caller
  must apply the trim.
- Apple `AudioConverter` MP3: +529 as well (the converter leaves the
  LAME-convention delay un-compensated). AAC priming instead comes from
  `kAudioConverterPrimeInfo`, captured at open and refreshed once after the
  first decoded chunk because AAC populates it only after decoding starts.
- Android `MediaCodec`: 0 — it surfaces no priming; `AndroidCodec` passes
  `TrackInfo.gapless` through verbatim and the standalone
  `AndroidMediaExtractorDemuxer` reports `gapless: None`.

`SymphoniaCodec::open_with_config` folds its own algo delay into the probed
`GaplessInfo` before exposing it through `track_info()`, so the audio pipeline
reads one fully-resolved trim. `Decoder::default_priming_frames` exposes the same
combined number for the `CodecPriming` fallback (MP3 = 576 + 529 = 1105) so
`kithara_audio::pipeline::gapless` need not know the backend. Measured: raw
Symphonia output of a libmp3lame sawtooth (`enc_delay` = 576) starts at sample
1105, raw Apple output at 576; both backends ignore the LAME tag, only the probe
reads it.

## Resampler integration

`kithara-resampler` owns resampler traits, config, and backend families (Rubato,
Glide, Apple); `kithara-decode` keeps only decoder-owned placement decisions.
`DecoderConfig::resampler` carries an optional `DecoderResamplerConfig<B>` — typed
backend, `target_sample_rate`, options, quality. Two placements:

- **Codec-embedded** — a backend that already owns a converter emits target-rate
  PCM directly (Apple, via `AppleCodec` over `AudioConverter`);
  `embedded_target_output_rate` decides whether the fused path is taken.
- **Standalone** — `resampled::wrap` builds the selected `ResamplerBackend` and
  wraps the decoder, owning interleaving, pooled planar scratch, target-rate
  metadata, and seek/gapless domain scaling. Skipped when the decoder already
  emits the target rate. Invalid backend/config pairs fail at construction — no
  trying another backend.

Shared AudioToolbox FFI (`AudioConverter`, `AudioFile`, `AudioBufferList`, POD
byte-copy wrappers) stays in `kithara-apple`; the standalone PCM-to-PCM Apple
backend stays in `kithara-resampler`. `kithara-decode` owns codec planning,
gapless policy, and the codec-embedded Apple decode path.

`Frames` and `Samples` (`pcm/units.rs`) keep the two PCM lengths apart. Planar
buffers are sized in frames, interleaved ones in samples, and the two differ by
the channel count — both are `usize`, and a buffer sized in the wrong one is
silent, off by exactly that factor. Conversion is explicit and needs the channel
count (`Frames::samples`, `Samples::frames`), so the multiply cannot be implied.
Applied where the two units meet — `ResampledDecoder::interleave` and
`PlayerResource::scratch_frames` — rather than to every frame count in the
workspace: inside a body where only one unit exists the type adds `get()` and no
guarantee. `PcmMeta.frames` stays a plain `u32`; it is a public field with no
interleaved counterpart beside it.

`sanitize_sample` is the workspace's one sample guard — `NaN`, infinities and
denormals all become silence — and `ResampledDecoder::append_chunk` applies it
as it deinterleaves, so a backend only ever sees finite normal input. A 32-bit
float file may legally hold any bit pattern, and a sinc backend spreads one
poisoned frame across its whole FIR window (a stateful one can hold it
indefinitely), so the guard belongs here: the adapter is the last owner of the
samples by value, and `Resampler::process_into_buffer` takes its input by
shared reference. `kithara-audio` reuses the same function at the two stages
that also take untrusted input — `IsolatorEq` and `PeakLimiter`. Pinned by
`resampler_never_sees_a_sample_the_file_poisoned`.

## Apple AAC input format (ESDS rationale)

The Apple `AudioConverter` accepts AAC via a magic cookie laid out as an ISO/IEC
14496-1 `ES_Descriptor`. Demuxers hand us either the raw `AudioSpecificConfig`
body (fMP4 / HLS; first byte = 5-bit AOT << 3, e.g. `0x10`–`0x17` for AAC LC) or a
full ESDS atom body (`AppleAudioFileDemuxer` reads
`kAudioFilePropertyMagicCookieData`, already a complete `ES_Descriptor` for M4A;
first byte = ESDS tag `0x03`). A single-byte sniff disambiguates without parsing:
`build_aac_input_format` wraps raw ASC into the minimum ESDS chain Apple accepts,
full ESDS bodies pass through unchanged, and the result mirrors
`AudioFileGetProperty(MagicCookieData)` for an `.m4a`:

```text
ES_Descriptor (tag 0x03):
  ES_ID (2 bytes) = 0; Flags (1 byte) = 0
  DecoderConfigDescriptor (tag 0x04):
    OTI (1 byte) = 0x40 (MPEG-4 Audio)
    StreamType (1 byte) = 0x15 (Audio << 2 | reserved bit)
    BufferSizeDB (3 bytes) = 0
    MaxBitrate (4 bytes) = 0
    AvgBitrate (4 bytes) = 0
    DecoderSpecificInfo (tag 0x05): <ASC bytes>
  SLConfigDescriptor (tag 0x06): predefined (1 byte) = 0x02
```

After cookie installation, `AudioFormatGetProperty(FormatList)` yields the
canonical ASBD for the first format item — that, not the demuxer's `TrackInfo`, is
authoritative for `mSampleRate` / `mChannelsPerFrame` / `mFramesPerPacket` (HE-AAC
v2 doubles the rate versus the container declaration; `FormatList` returns the
upsampled rate). FLAC mirrors the problem: `apple::flac::streaminfo_body`
normalises both carrier shapes (raw 34-byte STREAMINFO from the fMP4 `dfLa`
demuxer, full `FLACSpecificBox` cookie from `AudioFileServices`) to the body.

## WebCodecs host worker

Runtime initialization is main-thread owned. The doc-hidden FFI bootstrap
`spawn_webcodecs_probe` creates one host worker and probes the five compile-time
codec configurations through `AudioDecoder.isConfigSupported()`. The immutable
host sender and the completed support table publish through separate `OnceLock`s;
until the snapshot lands, `WebCodecsCodec::supports` returns `false` so the
synchronous factory takes the Symphonia path. Opening a WebCodecs codec before
runtime initialization is a typed contract error.

**A worker spawned by a blocked parent worker does not execute** — true for a
parent blocked by spinning and by `Atomics.wait`. The decode scheduler worker is
such a parked parent, so neither the codec nor the decode path may spawn the host;
only the main-thread bootstrap, whose event loop stays live, may. The host command
loop uses `try_recv` plus a local timer; the synchronous per-decoder reply receiver
uses the Atomics-backed `recv_timeout` path and needs no event loop. The singleton
host owns the decoder-ID → `AudioDecoder` map, reply channel, pending input queue,
and generation state; `Open` registers a codec, `Close` removes it. JavaScript
values never cross the Rust thread boundary.

The frame codec owns the current generation. A seek advances it, sends `Reset`,
discards queued output, then sends `Configure` again because WebCodecs `reset()`
returns `AudioDecoder` to the unconfigured state; commands and callbacks from
older generations are dropped everywhere. `WebCodecsCodec::needs_eof_drain` is
unconditionally `true` — the browser pipeline queues output at every rate. The
first empty frame sends `Flush`; each drain call waits within one bounded budget
for current-generation PCM, an explicit `Flushed`, or an error. Completion returns
zero frames; budget exhaustion is a typed backend error, never synthetic EOF. Seek
clears EOF-drain state. AAC priming and algorithmic delay stay at the `FrameCodec`
defaults.

Codec strings: `AacLc` → `mp4a.40.2`, `AacHe` → `mp4a.40.5`, `AacHeV2` →
`mp4a.40.29`, `Mp3` → `mp3`, `Flac` → `flac`. AAC `description` is the raw
`AudioSpecificConfig` from `TrackInfo::extra_data` — not an `esds` box or cookie;
MP3 has none; FLAC description is `fLaC` + a final-block STREAMINFO metadata
header + the 34-byte STREAMINFO payload from `TrackInfo`, and a non-34-byte
payload is a typed error. Capability probing needs a synthetic STREAMINFO, kept in
sync between `webcodecs/probe.rs` and `tests/webcodecs_browser.rs`.

Local browser lane (not part of CI; needs a working ChromeDriver):

```bash
CHROMEDRIVER="${CHROMEDRIVER:-chromedriver}" WASM_BINDGEN_TEST_TIMEOUT=300 \
WASM_BINDGEN_USE_BROWSER=1 cargo +nightly test --target wasm32-unknown-unknown \
  -p kithara-decode --no-default-features \
  --features symphonia,webcodecs,client-reqwest,tls-rustls --test webcodecs_browser
```

The HE-AAC v2 browser fixture is regenerated by the `#[ignore]`d
`generate_he_aac_v2_fixture` test in `tests/tests/kithara_hls/`.

## Feature flags

| Feature | Default | Effect |
| --- | --- | --- |
| `symphonia` | yes | Software decoder path |
| `fdk-aac` | yes | Override Symphonia's LC-only AAC decoder with the in-tree libfdk-aac adapter for HE-AAC v1/v2 (implies `symphonia`) |
| `resample-rubato` / `resample-glide` | rubato | Forward the resampler backend from `kithara-resampler` |
| `apple` | no | Apple `AudioToolbox` backend (gated on `target_os = "macos" \| "ios"`) |
| `apple-codec-embedded-resampler` | no | Select Apple codec-embedded resampling as the default placement (implies `apple`) |
| `android` | no | Android `MediaExtractor`/`MediaCodec` backend (gated on `target_os = "android"`) |
| `webcodecs` | no | Browser `AudioDecoder` backend (gated on `target_arch = "wasm32"`) |
| `probe` | no | USDT probes for tracing |
| `mock` | no | Expose `DecoderMock` (unimock) outside tests |
| `perf` | no | `hotpath` instrumentation (non-wasm) |
| `client-reqwest` / `client-wreq` | reqwest | Forward the HTTP backend selection to `kithara-stream` |
| `tls-rustls` / `tls-native` | rustls | Forward TLS selection to network-reaching deps |

With `symphonia` disabled (`default-features = false` plus only `apple` /
`android`), the factory has no software fallback — it errors if the active
hardware backend cannot handle a codec/container.

## Module layout

- `traits.rs` — public `Decoder`, typed outcomes, the `DecoderInput` supertrait
  (blanket impl over `Read + Seek + Send + Sync`), internal `BoxedSource`.
- `factory/` — `inner.rs` (config, factory, backend enum, per-backend dispatch),
  `probe.rs` (`ProbeHint`, extension/MIME/container maps, prefix sniffing); every
  backend is boxed into `Box<dyn Decoder>`.
- `composed.rs`, `demuxer.rs`, `codec.rs`, `resampled.rs` — decode loop, demuxer
  trait plus `TrackInfo`, `FrameCodec` / `CodecPriming` / `access_unit_frames`,
  standalone-resampler wrapper.
- `gapless/` — `info`, `heuristic`, `trimmer`, `probe`, `mp4`, `mp3` (Xing/Info +
  LAME, `skip_id3v2`). `mp4/` — streaming `moov` scanner (`scan_mp4`,
  `Mp4Visitor`, codec/fragmented sniffs). `fmp4/` — segment-by-segment demuxer,
  init parsing, source IO.
- `pcm_time.rs`, `types.rs`, `error.rs` — timeline math; PCM/track/profile types;
  `DecodeError` / `ErrorClass`. `mock.rs` re-exports `DecoderMock` under
  `cfg(test)` or the `mock` feature.
- `symphonia/` — `demuxer`, `codec`, `adapter` (`ReadSeekAdapter`: `Read+Seek` →
  `MediaSource`), `probe`, `registry`, `echain`, `aac_fdk` (feature `fdk-aac`).
  `apple/` (macOS/iOS), `android/`, `webcodecs/` (wasm32) — backend-local codec,
  demuxer, and platform-FFI modules.

## Trait bridges

- `From`: `&PcmMeta` → `kithara_stream::ChunkPosition`; `GaplessInfo` →
  `GaplessTrimmer`; `GaplessProbe` → `Option<GaplessInfo>` (prefers `elst` over
  `iTunSMPB`); `io::Error` / `TryFromIntError` / `BudgetExhausted` /
  `AndroidBackendError` → `DecodeError`.
- `TryFrom`: `DecoderChunkOutcome` → `PcmChunk`; `&[u8]` /
  `&AudioCodecParameters` → `AacStreamConfig` (fdk-aac).
- `Display`: `PcmSpec`, `DecoderBackend`.

## Tests

Cross-backend tests live in `kithara-integration-tests` under
`tests/tests/kithara_decode/` (`cargo nextest run -p kithara-integration-tests kithara_decode::`). `protocol_tests.rs` decodes the same MP3 with every available
backend and asserts agreement on `spec()`, `duration()`, total frame count,
post-seek timestamp, EOF semantics, and — when `apple` is enabled on macOS/iOS —
the full-decode PCM L2 norm within 2 %.
