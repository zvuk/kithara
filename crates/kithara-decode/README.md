<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-decode.svg)](https://crates.io/crates/kithara-decode)
[![docs.rs](https://docs.rs/kithara-decode/badge.svg)](https://docs.rs/kithara-decode)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-decode

Audio decoding library with explicit, typed backend selection. `DecoderFactory` creates synchronous `Decoder` instances that convert compressed audio (MP3, AAC, FLAC, WAV, ALAC, …) into pool-backed PCM (`Vec<f32>`). No threading, no channels — just decoding.

The public surface centres on one trait — `Decoder`. Concrete backends (Symphonia / Apple / Android) implement it directly. Internally, container parsing and frame decoding are split: the `Demuxer` trait owns container framing, the `FrameCodec` trait owns codec decoding, and `ComposedDecoder<D, C>` (internal) pairs them so backends can be mixed and matched. The factory hides this detail — callers only ever see `Box<dyn Decoder>`.

## Usage

```rust
use std::io::Cursor;
use kithara_decode::{DecoderBackend, DecoderConfig, DecoderFactory};

let reader = Cursor::new(wav_bytes);
let config = DecoderConfig { backend: DecoderBackend::Symphonia, ..Default::default() };
let mut decoder = DecoderFactory::create_with_probe(reader, Some("wav"), config)?;

let spec = decoder.spec(); // sample_rate, channels
loop {
    match decoder.next_chunk()? {
        kithara_decode::DecoderChunkOutcome::Chunk(chunk) => play(&chunk.pcm),
        kithara_decode::DecoderChunkOutcome::Pending(_) => continue,
        kithara_decode::DecoderChunkOutcome::Eof => break,
    }
}
```

For HLS / cross-codec recreate paths, prefer `DecoderFactory::create_from_media_info(reader, &media_info, config)` — it skips probing and uses the carried `MediaInfo` to pick the backend.

## Backends

<table>
<tr><th>Backend</th><th>Implementation</th><th>Platform</th></tr>
<tr><td>Symphonia</td><td>Software decoding; all formats</td><td>Cross-platform</td></tr>
<tr><td>Apple AudioToolbox</td><td>Hardware-accelerated; fMP4, ADTS, MP3, FLAC, CAF</td><td>macOS / iOS</td></tr>
<tr><td>Android MediaCodec</td><td>Runtime hardware path for AAC-LC, MP3, FLAC with recoverable fallback to Symphonia</td><td>Android</td></tr>
</table>

## Initialization Paths

1. **Direct reader creation** (`container` specified): Creates format reader directly without probing. Used for HLS fMP4 where format is known but byte length is unknown. Seek is disabled during init to prevent `IsoMp4Reader` from seeking to end.
2. **Probe** (`container` not specified): Uses Symphonia's auto-detection. Supports `probe_no_seek` for ABR variant switches where reported byte length may not match.

## Decoder recreate strategy

Seek-time and variant-switch rebuilds go through `create_from_media_info`,
which skips probing and selects the backend from the carried `MediaInfo`.
**No fallback**: if the metadata-driven path fails, the error propagates
verbatim. Probing mid-segment bytes at a mismatched offset can silently
match an unrelated codec (e.g. an MP3 frame sync inside raw AAC-in-fMP4
bytes) and drive the rest of the pipeline off a codec the decoder never
actually decoded — so the recreate path never probes.

## Decoder input contract

Each demuxer declares the *shape* of input it needs to be constructed via
`Demuxer::required_input() -> InputRequirement` (default `Incremental`;
`Fmp4SegmentDemuxer` overrides it). The value is a reading *discipline*, not a
byte window — the concrete init range is resolved by the byte-space owner (the
stream layer), which alone knows the ABR virtual-space byte shift.

- `InitOnly` — segment-aware fMP4: the init header (moov/esds/STREAMINFO) must
  be buffered before construction; the landing media segment is read later by the
  first `next_frame` and pends until it arrives, so it is not a build
  prerequisite (gating on it would invert build-then-pend into a circular
  dependency). Both backends gate identically because both wrap
  `Fmp4SegmentDemuxer`.
- `Incremental` — single-file MP3/FLAC/Ogg, Apple `AudioFile`, Android
  `MediaExtractor`: self-framing, no separate init, so nothing is gated up front;
  the demuxer reads and pends as bytes arrive.

`DecoderFactory::input_requirement(media_info, byte_map)` is the bridge to the
kithara-audio readiness gate: it returns the contract of the demuxer the factory
*would* build (mirroring `should_use_segment_aware`), which the gate then
resolves to concrete bytes in its own virtual coordinate space.

## Gapless playback

`DecoderConfig::gapless` is enabled by default. Decoders report engine-level trim
metadata through `DecoderTrackInfo::gapless: Option<GaplessInfo>`, where
`leading_frames` and `trailing_frames` are PCM frame counts.

The contract has one owner for actual trimming:

- `Some(GaplessInfo)` means the backend decoded the untrimmed PCM region and the
  `kithara-audio` pipeline must apply `GaplessTrimmer` before effects.
- `None` means no engine trim should run. This covers files with no gapless
  metadata and backend paths that already applied gapless trim internally.
- `GaplessTrimmer::notify_seek()` drops seek-sensitive state — leading trim,
  pending fade-in, and the buffered tail. Trailing trim still applies at EOF.

When metadata is absent, `kithara-audio`'s `AudioConfig::gapless_mode` can select
heuristic behaviour via `GaplessMode`:

- `GaplessMode::CodecPriming` — `GaplessTrimmer::codec_priming(frames, sample_rate)`
  is built from a static codec table (`codec_priming_frames`). AAC LC
  is 2112, HE-AAC 3072, MP3 LAME-default 1105, Opus 312, and lossless
  codecs are 0. Predictable and zero-latency.
- `GaplessMode::SilenceTrim(SilenceTrimParams)` — `GaplessTrimmer::silence_trim`
  walks the leading buffer until the first sample above a configurable
  dB threshold and trims everything before it. Optionally trims the
  trailing silence at EOF too.

See also `GaplessMode::Disabled` and `GaplessMode::MediaOnly` on `AudioConfig`.

Both fallbacks apply a short raised-cosine fade-in (~3 ms) at the trim
boundary. The metadata-driven path does not — the boundary lands on a
sample-accurate count.

## Seek pre-roll and trim

Seeking is a two-step contract shared by the demuxer and `ComposedDecoder`:

1. **Demuxer back-off (pre-roll).** `Demuxer::seek` parks the cursor
   *before* the user-requested target. The back-off magnitude comes from
   `FrameCodec::priming(codec)` (a `CodecPriming`) and covers two needs:
   - **Codec warm-up.** MDCT/SBR/PS codecs must decode pre-target packets
     to converge overlap-add and QMF state, otherwise a cold flush leaks
     artifacts. AAC (incl. HE-AAC v1/v2, which the fMP4 init parse and the
     codec layer both see only as `AacLc` — fdk-aac auto-detects SBR from
     the bitstream) requests **2 access units** of pre-roll; a plain-LC
     stream just decode-discards one harmless extra AU. Codecs that
     converge instantly, or that Symphonia primes internally (MP3, FLAC,
     …), keep the empty `CodecPriming::default()`.
   - **At least one whole codec packet** so the trim step below lands on a
     packet boundary. Back-off is derived from codec facts (1024
     frames/packet for AAC, 1152 for MP3) and the track `sample_rate` — no
     magic millisecond constants.

   For fMP4 the back-off (`target − warmup`) can cross a segment boundary;
   `Fmp4SegmentDemuxer::seek` then decodes the tail of the prior segment as
   decode-and-discard warm-up so SBR converges across the boundary instead
   of starting cold. Reads stay confined to the pre-roll segment plus the
   target segment — never a prefix walk from seg-0.

2. **Sample-accurate trim.** Landing on a packet boundary means the first
   decoded frame straddles the target. `ComposedDecoder::pending_seek_target`
   drops whole pre-target frames, then trims the leading samples of the
   straddling frame (`frames_to_trim`, round-to-nearest sample) so the
   emitted chunk starts exactly at `target` rather than at the packet
   boundary. Without this trim a seek leaks up to one packet of pre-target
   audio.

Current metadata sources:

- AAC in MP4/M4A/fMP4: MP4 probe reads `edts/elst` first, then falls back to
  `iTunSMPB`.
- MP3, FLAC, Vorbis, and Opus through Symphonia rely on the backend's own
  gapless behavior and therefore expose `None` for engine trim.
- Apple AudioToolbox captures `AudioConverterPrimeInfo` when available.
- Android MediaCodec reads `encoder-delay`/`encoder-padding` from `MediaFormat`
  and falls back to the MP4 probe for AAC MP4 containers.

## Read-ahead strand

When the source is an HLS `Stream`, a `next_frame` read can be interrupted at a
not-yet-downloaded segment boundary. Symphonia's `MediaSourceStream` (MSS)
buffers read-ahead in a ring and consumes bytes from that ring into a packet
buffer *before* it knows whether the read can complete. If a later internal
`fetch` then hits the not-ready boundary and returns `Interrupted`, the
half-read packet is discarded but MSS's read position has already advanced past
the consumed bytes — they are **stranded**. On resume, byte-position-quantised
readers (WAV/PCM, whose packet pts is derived from the stream position) compute
the next packet's pts from the advanced position and silently skip the stranded
bytes (~one packet of decoded PCM).

`MSS` exposes no per-call rewind through the `FormatReader` trait, so
`SymphoniaDemuxer` makes the **decoder's timestamp authoritative across a
`Pending`**: it tracks `resume_ts` (native timebase units — `actual_ts` on seek,
`pts + dur` after each emitted packet) and, on any interrupted read, sets
`needs_resume`. The next `next_frame` re-seeks the reader to `resume_ts` before
reading, so the interrupted packet is re-read from its start instead of skipped.
The re-seek is a bare position restore — no pre-roll back-off and no codec flush
(that is the user-seek path) — and is idempotent when no strand occurred (the
read position already sits at `resume_ts`). `resume_ts` is kept in native units,
not `Duration`, because a `Duration` round-trip loses sub-frame precision and
snaps the re-seek one packet early (re-emitting a packet instead of continuing).
This keeps the strand contained in the decode layer: it never reaches into the
`Stream`/`wait_range` contract.

## Feature Flags

<table>
<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>
<tr><td><code>symphonia</code></td><td>yes</td><td>Software decoder path (Symphonia)</td></tr>
<tr><td><code>apple</code></td><td>no</td><td>Apple AudioToolbox hardware decoder (gated on <code>target_os = "macos" | "ios"</code>)</td></tr>
<tr><td><code>android</code></td><td>no</td><td>Android <code>MediaExtractor</code>/<code>MediaCodec</code> hardware decoder (gated on <code>target_os = "android"</code>)</td></tr>
<tr><td><code>probe</code></td><td>no</td><td>USDT probes for tracing</td></tr>
<tr><td><code>mock</code></td><td>no</td><td><code>unimock</code>-generated mocks of the public traits</td></tr>
<tr><td><code>perf</code></td><td>no</td><td>Performance instrumentation via <code>hotpath</code></td></tr>
</table>

When `symphonia` is disabled (`default-features = false` + only `apple` / `android`), the factory has no software fallback — it errors if the active hardware backend cannot handle a codec/container.

## Module layout

- `src/traits.rs` — public `Decoder` trait plus typed outcomes (`DecoderChunkOutcome`, `DecoderSeekOutcome`, `InputReadOutcome`) and the `DecoderInput` source supertrait.
- `src/factory/` — `DecoderConfig`, `DecoderFactory`, and the `DecoderBackend` selector enum. The factory boxes every backend into `Box<dyn Decoder>` so callers stay codec-agnostic.
- `src/composed.rs` — internal `ComposedDecoder<D: Demuxer, C: FrameCodec>` that implements `Decoder` by pairing a demuxer with a frame-level codec.
- `src/demuxer/` — internal `Demuxer` trait and concrete demuxers (fMP4, …).
- `src/fmp4/`, `src/mp4.rs` — fMP4/MP4 container helpers.
- `src/symphonia/` (feature `symphonia`) — Symphonia `Decoder` implementation; probe and direct paths; `ReadSeekAdapter`.
- `src/apple/` (feature `apple`, macOS / iOS) — Apple `AudioToolbox` backend over `AudioFile` / `AudioConverter` FFI.
- `src/android/` (feature `android`, Android) — `MediaExtractor` / `MediaCodec` backend over JNI.
- `src/gapless/` — `GaplessInfo`, `GaplessMode`, `GaplessTrimmer`, `SilenceTrimParams`, the encoder-side `probe_codec_gapless`, MP4 `udta`/`iTunSMPB` / MPEG-audio Xing/LAME tag parsers, and the trailing fade/silence trim heuristics.
- `src/pcm_time.rs` — timeline math (`duration_for_frames`, `frames_for_duration`, PTS helpers) shared across backends.
- `src/types.rs`, `src/error.rs` — shared types and `DecodeError` / `ErrorClass`.

## Cross-decoder protocol test

The cross-decoder protocol test lives in `kithara-integration-tests` under `tests/tests/kithara_decode/`. It decodes the same MP3 with every available backend and asserts agreement on `spec()`, `duration()`, total frame count, post-seek timestamp, EOF semantics, and (when the `apple` feature is enabled on macOS/iOS) the full-decode PCM L2 norm within 2 %.

```bash
cargo nextest run -p kithara-integration-tests kithara_decode::
```

## Gapless probe contract

The gapless pipeline splits "where does silence come from?" into two
independent layers:

1. **Encoder-side priming / padding** — silence the *encoder* added at
   compress time. `probe_codec_gapless` reads container metadata
   (`iTunSMPB` / `elst` for AAC LC inside MP4, Xing/Info+LAME for MP3
   inside MPEG audio) and returns `Some(GaplessInfo)` when it found
   real values. **No fallback chains** — when metadata is absent the
   probe returns `None` and the pipeline falls back through
   `GaplessMode::CodecPriming` to `AudioCodec::encoder_priming_frames`
   (libmp3lame default 576, AAC LC MDCT block 1024, Opus RFC pre-skip
   312, others 0).

2. **Decoder-side algorithmic delay** — silence the *decoder* itself
   emits before it converges. Lives on `FrameCodec::decoder_algo_delay`
   and is per-backend:
   - Symphonia `mpa` (LAME convention): +529 leading, −529 trailing
     for MP3. Symphonia's own demuxer parses the LAME tag and sets
     `track.delay = enc_delay + 528 + 1`, but the 0.6.0-alpha.1
     demuxer does NOT populate per-packet `trim_start` / `trim_end`,
     so the decoder's `opts.gapless` flag is a no-op for MP3 — the
     caller must apply the trim.
   - Apple `AudioConverter` MP3: +0 (internally compensated; the
     converter emits exactly `enc_delay` samples of leading silence).
   - Android `MediaCodec`: +0 (no surfaced priming; metadata comes
     from the demuxer's MP4 udta probe).

`SymphoniaCodec::open_with_config` folds its own algo delay into the
probed `GaplessInfo` before exposing it through `track_info()`; the
audio pipeline reads one fully-resolved trim and forgets the layered
origin. `Decoder::default_priming_frames` exposes the same combined
number for the `CodecPriming` fallback so
`kithara_audio::pipeline::gapless::resolve_codec_priming` does not
need to know which backend it is talking to.

Empirical justification: raw Symphonia output of a libmp3lame
sawtooth (`enc_delay` = 576) starts at sample 1105 = 576 + 529; raw
Apple output of the same fixture starts at sample 576. Both backends
ignore the LAME tag entirely — verified by patching `enc_delay` in the
tag to arbitrary values (0 / 100 / 1152 / 2400); leading silence in
output stayed constant. The probe is the only thing that reads the
tag, then the codec adds (or doesn't add) its own algorithmic delay.

## Apple AAC input format (ESDS rationale)

The Apple `AudioConverter` accepts AAC via a magic cookie whose layout
is the ISO/IEC 14496-1 `ES_Descriptor`. Demuxers can hand us either
the raw `AudioSpecificConfig` body (fMP4 / HLS path, first byte =
5-bit AOT << 3, e.g. `0x10`–`0x17` for AAC LC) or the full ESDS atom
body (`AppleAudioFileDemuxer` reads `kAudioFilePropertyMagicCookieData`
which for M4A is already a complete ES_Descriptor; first byte = ESDS
tag `0x03`). A single-byte sniff disambiguates without parsing —
`build_aac_input_format` wraps raw ASC into the minimum ESDS chain
Apple's `AudioFormat` / `AudioConverter` APIs accept; full ESDS bodies
go through unchanged.

The ESDS shape we produce mirrors what `AudioFileGetProperty(MagicCookieData)`
returns for an `.m4a` file:

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

After cookie installation we ask `AudioFormatGetProperty(FormatList)`
to derive the canonical ASBD for the first format item — that is the
authoritative `mSampleRate` / `mChannelsPerFrame` /
`mFramesPerPacket`, not whatever the demuxer reported in `TrackInfo`
(HE-AAC v2 doubles the rate vs the container declaration; `FormatList`
returns the upsampled rate).

## Trait Bridges

- `&PcmMeta` → `kithara_stream::ChunkPosition` (`From`) — map decoded-chunk timing into stream space
- `DecoderChunkOutcome` → `PcmChunk` (`TryFrom`) — extract PCM from a decode outcome
- `GaplessProbe` → `Option<GaplessInfo>` (`From`) — prefer elst-derived over iTunSMPB gapless data
- `GaplessInfo` → `GaplessTrimmer` (`From`) — build the encoder-delay/padding trimmer
- `&[u8]` / `&AudioCodecParameters` → `AacStreamConfig` (`TryFrom`) — FDK AAC config from ASC or codec params
- `io::Error` / `TryFromIntError` / `BudgetExhausted` → `DecodeError` (`From`) — typed error wrapping
- `PcmSpec` / `DecoderBackend` (`Display`) — human-readable rendering

## Integration

Consumed by `kithara-audio` which wraps it in a threaded pipeline with effects and resampling. Accepts any `R: Read + Seek + Send + Sync + 'static` -- works with `Stream<File>`, `Stream<Hls>`, `Cursor<Vec<u8>>`, or plain files.
