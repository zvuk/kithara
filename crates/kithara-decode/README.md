<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-decode

Audio decoding library with explicit, typed backend selection. `DecoderFactory` creates synchronous `Decoder` instances that convert compressed audio (MP3, AAC, FLAC, WAV, ALAC, …) into pool-backed PCM (`Vec<f32>`). No threading, no channels — just decoding.

The public surface centres on one trait — `Decoder`. Concrete backends (Symphonia / Apple / Android) implement it directly. Internally, container parsing and codec stepping are split: the `Demuxer` trait owns container framing, the `FrameCodec` trait owns codec decoding, and `ComposedDecoder<D, C>` (internal) wires them together so backends can be mixed and matched. The factory hides this detail — callers only ever see `Box<dyn Decoder>`.

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
<tr><td>Android MediaCodec</td><td>Runtime hardware path for AAC family, MP3, FLAC with recoverable fallback to Symphonia</td><td>Android</td></tr>
</table>

## Initialization Paths

1. **Direct reader creation** (`container` specified): Creates format reader directly without probing. Used for HLS fMP4 where format is known but byte length is unknown. Seek is disabled during init to prevent `IsoMp4Reader` from seeking to end.
2. **Probe** (`container` not specified): Uses Symphonia's auto-detection. Supports `probe_no_seek` for ABR variant switches where reported byte length may not match.

## Decoder recreate strategy

- `create_for_recreate` is used for seek-time decoder rebuild.
- It is a thin wrapper over `create_from_media_info`: callers must
  supply a `base_offset` that lines up with the container's init
  region (for fMP4/MP4/WAV/MKV/CAF the `ftyp`/RIFF/EBML header; for
  MPEG-ES / ADTS / FLAC / Ogg / MPEG-TS any valid packet start).
- **No fallback**: when the metadata-driven path fails the error is
  propagated verbatim. Probing mid-segment bytes at a mismatched
  offset can silently match an unrelated codec (e.g. MP3 frame sync
  in raw AAC-in-fMP4 bytes) and drive the rest of the pipeline off a
  `session.media_info` the decoder never actually realised.

## Gapless playback

`DecoderConfig::gapless` is enabled by default. Decoders report engine-level trim
metadata through `DecoderTrackInfo::gapless: Option<GaplessInfo>`, where
`leading_frames` and `trailing_frames` are PCM frame counts.

The contract has one owner for actual trimming:

- `Some(GaplessInfo)` means the backend decoded the untrimmed PCM region and the
  `kithara-audio` pipeline must apply `GaplessTrimmer` before effects.
- `None` means no engine trim should run. This covers files with no gapless
  metadata and backend paths that already applied gapless trim internally.
- `GaplessTrimmer::notify_seek()` drops only the leading trim state; tail trim is
  still applied at EOF for the current track.

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

`GaplessTrimmer::notify_seek()` drops both the leading-trim state and
any pending fade-in; tail trim continues to be applied at EOF.

Current metadata sources:

- AAC in MP4/M4A/fMP4: MP4 probe reads `edts/elst` first, then falls back to
  `iTunSMPB`.
- MP3, FLAC, Vorbis, and Opus through Symphonia rely on the backend's own
  gapless behavior and therefore expose `None` for engine trim.
- Apple AudioToolbox captures `AudioConverterPrimeInfo` when available.
- Android MediaCodec reads `encoder-delay`/`encoder-padding` from `MediaFormat`
  and falls back to the MP4 probe for AAC MP4 containers.

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

To exercise the same protocol per backend in isolation (no cross-feature unification):

```bash
just test-decoders
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

## Integration

Consumed by `kithara-audio` which wraps it in a threaded pipeline with effects and resampling. Accepts any `R: Read + Seek + Send + Sync + 'static` -- works with `Stream<File>`, `Stream<Hls>`, `Cursor<Vec<u8>>`, or plain files.
