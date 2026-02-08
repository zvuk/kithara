# ABR Switch Click Fix

## Problem

When switching between HLS variants (e.g., AAC 66kbps -> FLAC lossless), an audible
click/pop occurs at the transition boundary.

## Root Cause

Different audio codecs produce different amounts of **encoder delay** (leading silence)
in their decoded PCM output:

| Variant   | Codec | Encoder Delay | Duration |
|-----------|-------|---------------|----------|
| slq       | AAC   | 2089 frames   | 47.4ms   |
| smq       | AAC   | 2080 frames   | 47.2ms   |
| shq       | AAC   | 2096 frames   | 47.5ms   |
| slossless | FLAC  | 1062 frames   | 24.1ms   |

This delay is a property of the codec encoder configuration. It's important to
distinguish **decoder delay** from **encoder delay**:

- **Decoder delay** (1024 samples): algorithmic minimum for AAC. The codec uses a
  transform over 2048 samples applied every 1024 samples (overlapped). Both consecutive
  transforms are needed to decode any 1024-sample period — hence the fixed 1024-sample
  minimum delay.

- **Encoder delay** (typically 2112 samples): the actual amount of silent priming
  prepended by the encoder. Always ≥ 1024 (the decoder delay minimum), but encoders
  typically add more. Apple's standard is 2112 = 2 × 1024 + 64.

FLAC has no transform overlap but still has ~1062 frames of leading silence in this
stream — an artifact of the specific encoder/container combination.

**The click has two components:**

1. **Priming zeros on cold-start:** When Symphonia recreates the decoder for a new variant,
   the first decoded samples are near-zero (encoder priming). This creates a discontinuity
   from the previous audio signal.

2. **Temporal misalignment:** AAC and FLAC produce different total sample counts for the
   same audio content. Over 37 segments: AAC = 9,712,640 frames, FLAC = 9,710,657 frames
   (difference: 1,983 frames). The main offset is the fixed encoder delay difference
   (~1027 frames between AAC and FLAC), not per-segment accumulation.

## How Apple AVPlayer Handles This

### Enabling Lossless Audio Variants

AVPlayer requires `AVVariantPreferenceScalabilityToLosslessAudio` (macOS 11.3+/iOS 14.5+)
to consider lossless variants during ABR selection:

```swift
item.variantPreferences = .scalabilityToLosslessAudio
```

**Without** this flag, AVPlayer's ABR engine stays within the same codec family
(AAC→AAC only). Even with `preferredPeakBitRate` set high enough, it selects shq
(AAC 270k) instead of slossless (FLAC 989k).

### What We Verified

**1. HLS playback with ABR switch (macOS 15):**
- AVPlayer switches AAC→FLAC when `scalabilityToLosslessAudio` is set
- Access log: 37 AAC segments → 35 FLAC segments, **0 stalls** (no buffer underruns)
- Playback position stays continuous — no audible gap at transition
- PCM capture impossible: `MTAudioProcessingTap` callbacks never fire for HLS on
  macOS 15 (deprecated since macOS 13, appears non-functional for HLS streams)

**2. Local fMP4 via AVQueuePlayer (macOS 15):**
- `MTAudioProcessingTap` **does work** with local fMP4 files (not HLS)
- Captures PCM correctly: tap prepare/process callbacks fire
- Cross-codec transition (separate AAC→FLAC files) produces **SEVERE click** (0.71
  discontinuity) — AVQueuePlayer does NOT do gapless cross-codec transitions
- Leading silence: 5120 frames (116ms) — encoder delay not trimmed

**3. Apple AudioToolbox decoder (via kithara-decode):**
- Produces **identical encoder delay values** as Symphonia (2089, 2080, 2096, 1062)
- `kAudioConverterPrimeInfo` returns zeros for all codecs (no `elst` in our files)
- Same `delay_diff` (+1027) and `xcorr` (+3..+4) alignment as Symphonia
- AudioToolbox does NOT auto-trim encoder delay without `elst` metadata

**Conclusion:** AVPlayer's cross-codec gapless transitions are handled by the internal
HLS engine, not by the audio decoder or AudioToolbox. The mechanism is closed-source
and not available via public API. Our implementation must handle this ourselves.

## Encoder Delay: Industry Standards

### Apple TN2258 — Implicit Encoder Delay

Apple's "historical solution" (TN2258 / QuickTime Appendix G): when no explicit metadata
is present, assume **2112 samples** of encoder delay for AAC.

Breakdown: 2 full AAC frames (2 × 1024 = 2048) + 64 samples = 2112.

| Encoder              | Priming Samples |
|----------------------|-----------------|
| iTunes/QuickTime     | 2112            |
| libfdk_aac           | 2112            |
| FFmpeg native aac    | 1024            |
| Nero AAC-LC          | 2624            |
| Nero HE-AAC          | 2336            |
| **Our encoder (slq)**| **2089**        |
| **Our encoder (smq)**| **2080**        |
| **Our encoder (shq)**| **2096**        |

Our encoder produces delays that **don't match any standard value**.
Delta from Apple implicit: -23 (slq), -32 (smq), -16 (shq).

### Explicit Metadata Mechanisms

Apple documentation (Appendix G) defines that a **complete** explicit encoder delay
representation requires **three** atoms working together. The edit list alone is not
sufficient:

> "You cannot use the edit list by itself to determine the encoder delay or remainder
> sample count. The sample group atoms provide the encoder delay."

#### 1. `elst` (Edit List) — ISO 14496-12 §8.6.6

The `media_time` field specifies the encoder delay offset; `track_duration` specifies
the source waveform duration. Example from Apple docs:

```
elst: entries=1, media_time=2112, track_duration=240000, rate=1.0
```

- Symphonia parses `elst` but stores it as `#[allow(dead_code)]`
- **Our init segments do NOT contain `edts`/`elst` boxes**

#### 2. `sgpd` (Sample Group Description)

Grouping type `'roll'` with payload (roll distance) = -1, meaning "1 preceding AAC
packet needed for decoding" = 1024 samples decoder delay. Its presence signals that
encoder delay info in this track is **explicit** (not the implicit 2112 assumption).

```
sgpd: version=1, grouping_type='roll', default_length=2, entry_count=1, payload=-1
```

- Symphonia does not implement
- **Not present in our init segments or media segments**

#### 3. `sbgp` (Sample-to-Group)

Associates all AAC packets with the group description from `sgpd`.

```
sbgp: version=0, grouping_type='roll', entry_count=1, sample_count=237, group_index=1
```

- Symphonia does not implement
- **Not present in our init segments or media segments**

#### Without `sgpd`/`sbgp` → implicit 2112 fallback

Apple documentation states: "Incomplete implementations may produce unpredictable
results; absent structures default to implicit 2112-sample delay assumptions."

#### 4. `iTunSMPB` — Apple proprietary

Atom in `moov/udta/meta/ilst`. Format:
`[reserved] [priming] [remainder] [original_samples] [reserved]`
Example: `00000000 00000840 00000278 000000000021A548 00000000`
(priming=2112, remainder=632, original=2205000)

- **Not present in our init segments**

### kAudioConverterPrimeInfo (AudioToolbox)

We tested querying `kAudioConverterPrimeInfo` from AudioToolbox's `AudioConverter`:

```
slq  (AAC):  leading=0  trailing=0
smq  (AAC):  leading=0  trailing=0
shq  (AAC):  leading=0  trailing=0
slossless (FLAC): leading=0  trailing=0
```

**Result: all zeros.** `PrimeInfo` is a **writable** property — the demuxer reads `elst`
from the container and *sets* it on the converter. The converter itself does not
intrinsically know its priming. Without `elst`, PrimeInfo stays at zero.

### FLAC Has No Standard Encoder Delay

FLAC specification does not define an encoder delay field. The 1062 frames of silence
in our stream are an artifact of the specific encoder/container combination, not a
codec property.

### Container Structure (Our Init Segments)

Verified by parsing all init.mp4 files (both init segments and media segments):

```
Init: ftyp → moov → { mvhd, mvex/trex, trak/{ tkhd, mdia/{ mdhd, hdlr, minf/stbl }}}
Seg:  styp → sidx → moof → mdat
```

- **timescale**: 90000
- **AAC AudioSpecificConfig**: AOT=2 (AAC-LC), freq=44100Hz, ch=2, frame_len=1024
- **No `edts`/`elst` boxes** — encoder delay not signaled
- **No `iTunSMPB`** — no Apple gapless metadata
- **No `sgpd`/`sbgp`** — no sample group info (checked both init and media segments)

None of the three atoms required for explicit encoder delay representation are present.
The encoder that produced these files does not write gapless metadata.

`trun` boxes contain `sample_duration` and `sample_size` per sample but no priming info.
`base_decode_time` is purely cumulative.

## What Didn't Work

### 1. Skip fixed number of priming frames
Skip first 1024 frames from cold-start decoder. **Result:** inconsistent - sometimes
helps, sometimes makes it worse. The actual priming count varies and 1024 is arbitrary.

### 2. Simple crossfade at boundary
20ms linear crossfade between old decoder's tail and new decoder's head.
**Result:** doesn't help because the signals being crossfaded are fundamentally different
(different codecs, different temporal positions). Crossfading between misaligned signals
produces a delay/echo effect.

### 3. Overlap decode with segment-number alignment
Decode transition segment with both decoders, align by segment number.
**Result:** delay/echo effect because AAC segment N and FLAC segment N don't represent
the same temporal position in PCM (encoder delay accumulates differently).

### 4. Overlap decode with length-ratio alignment
Use ratio of total PCM lengths to estimate corresponding positions.
**Result:** still has delay effect because the offset is primarily a fixed encoder delay,
not a proportional drift.

### 5. Apple implicit delay (2112 for AAC, 0 for FLAC)
Use Apple's assumed 2112-sample AAC delay and 0 for FLAC, then crossfade.
**Result:** delay_diff = 2112 - 0 = 2112, but real difference is 1027.
Error of **1085 samples (~24.6ms)** — too large for crossfade to absorb.
Cross-correlation tries to compensate but hits ±128 frame search limit.

Simulator results for AAC→FLAC:
```
silence_detect: delay_diff=+1027  xcorr=+3   → correct alignment
apple_implicit: delay_diff=+2112  xcorr=+128 → maxed out, still misaligned by ~957 frames
```

This approach fails for cross-codec switches because FLAC has no standard implicit delay,
and the Apple 2112 value doesn't match our encoder anyway (actual: 2089).

## Working Solution (Simulator)

**Encoder delay measurement + cross-correlation alignment + short crossfade.**

### Algorithm

1. **Decode both variants continuously** from segment 1 through the transition segment.
   Both decoders are "warm" (no cold-start priming artifacts).

2. **Measure encoder delay** of each variant by counting leading near-silent frames
   (threshold < 0.001) in the continuous decode output.

3. **Compute delay difference:** `delay_diff = old_delay - new_delay`
   (e.g., 2089 - 1062 = 1027 frames for AAC -> FLAC)

4. **Initial alignment:** `new_pos = old_pos - delay_diff`
   This compensates for the fixed encoder delay offset.

5. **Fine-tune with cross-correlation:** Search +/-128 frames around the estimated
   position using cross-correlation on a 2048-frame window. This corrects for
   per-segment sample count variations.

6. **Short crossfade (20ms)** between old signal and aligned new signal.

### Key Insight

The encoder delay difference is approximately **fixed** across the stream (not cumulative).
The per-segment drift is small (~54 frames/segment, ~1.2ms). The main correction is the
one-time encoder delay offset, refined by cross-correlation.

### Verified Results

Tested at segments 2, 6, 18 in both directions:

**AAC 66k → FLAC:**
- `delay_diff = +1027` frames consistently
- `xcorr = +3..+4` frames fine adjustment
- No audible click in generated WAV files

**FLAC → AAC 66k:**
- `delay_diff = -1027` frames (symmetric)
- `xcorr = -3` frames fine adjustment
- No audible click in generated WAV files

## Production Implementation Plan

### Overview: "Old Decoder Overshoot" Strategy

The old decoder is warm and continuous — no priming artifacts. Instead of immediately
destroying it at ABR switch, let it **decode one more segment** of the old variant.
This gives us clean PCM for the transition period. Then cold-start the new decoder,
skip its priming, and crossfade between the two.

```
Timeline:

Seg N-1 (old)    Seg N (old, overshoot)     Seg N+1 (new)
[=====A=====]    [=====A (buffered)=====]    [========B========]
                 [===B (cold, priming)===]
                      ↓ crossfade ↓
                 [====blended output====]    [========B========]
```

### Step-by-step

1. **ABR switch detected** at segment N boundary (format change in `media_info()`).

2. **Overshoot**: old decoder continues, decodes segment N of the OLD variant.
   The segment data is already downloaded (old variant's segment N).
   Output: clean warm PCM, buffered as `overshoot_pcm`.

3. **New decoder created**, cold-starts on segment N of the NEW variant.
   First decoded chunk contains priming frames (encoder delay zeros).

4. **Skip priming** from new decoder's output: count leading near-zero frames
   (< 0.001 threshold) in the first chunk. This is the encoder delay of the
   new codec. Skip those frames.

5. **Crossfade** (20ms) from `overshoot_pcm` tail into new decoder's output
   (after priming skip). Both signals represent the same time period
   (segment N), so they are approximately aligned. Per-segment sample count
   difference is ~1.2ms — absorbed by the 20ms crossfade window.

6. **Continue** playing new variant from segment N+1 normally.

### Runtime Encoder Delay Detection

No hardcoded constants. Encoder delay is measured at runtime:

- **When**: at each cold-start decode (new decoder's first chunk)
- **How**: count leading frames where `max(|sample|) < threshold` across all channels
- **Threshold**: 0.001 (proven reliable: 2089 for AAC, 1062 for FLAC)
- **Usage**: skip that many frames from cold-start output before crossfade

#### Why silence detection over alternatives

| Approach               | Accuracy         | Universality     | Status            |
|------------------------|------------------|------------------|-------------------|
| `elst`+`sgpd`+`sbgp`  | Exact (if all 3) | AAC in MP4/fMP4  | Not in our files  |
| `elst` alone           | Ambiguous*       | AAC only         | Not in our files  |
| `iTunSMPB`             | Exact (if exists)| Apple only       | Not in our files  |
| Apple implicit (2112)  | ±48 samples      | AAC only         | No FLAC standard  |
| `PrimeInfo` API        | Needs `elst`     | Apple platforms   | Returns zeros     |
| **Silence detection**  | **Exact**        | **Any codec**    | **Works now**     |

\* Apple docs: "You cannot use the edit list by itself to determine the encoder delay."

### Architecture

#### Where each piece lives

```
StreamAudioSource (kithara-audio/src/pipeline/stream_source.rs)
  ├── crossfader: Crossfader                  — owns transition state
  ├── detect_format_change()                  — detects ABR switch
  ├── apply_format_change()                   — orchestrates transition
  │     ├── old decoder decodes overshoot segment
  │     ├── crossfader.set_overshoot(pcm, spec)
  │     └── create new decoder
  └── fetch_next()
        └── crossfader.process(chunk)         — blends if overshoot pending

Crossfader (kithara-audio/src/pipeline/crossfade.rs)
  ├── overshoot: Option<(Vec<f32>, PcmSpec)>  — buffered old decoder output
  ├── set_overshoot(pcm, spec)                — store buffer from old decoder
  ├── process(chunk) -> PcmChunk              — skip priming + crossfade if pending
  └── reset()                                 — clear on seek
```

The crossfade lives **above the decoder** in a dedicated struct. `StreamAudioSource`
calls two methods: `set_overshoot()` during ABR switch, `process()` on every chunk.

#### Crossfader API

```rust
pub struct Crossfader {
    overshoot: Option<(Vec<f32>, PcmSpec)>,
}

impl Crossfader {
    pub fn new() -> Self;

    /// Store overshoot PCM from old decoder before destroying it.
    pub fn set_overshoot(&mut self, pcm: Vec<f32>, spec: PcmSpec);

    /// Process a chunk from the decoder.
    /// - No overshoot pending: return chunk unchanged.
    /// - Overshoot pending: skip priming in chunk, crossfade from
    ///   overshoot tail into chunk head, clear overshoot.
    pub fn process(&mut self, chunk: PcmChunk<f32>) -> PcmChunk<f32>;

    /// Clear state (on seek).
    pub fn reset(&mut self);
}
```

Priming detection and crossfade are private implementation details inside `process()`.

#### Changes to StreamAudioSource

In `apply_format_change()`:
1. Before destroying old decoder, let it decode one more segment
   (the overshoot segment of the OLD variant)
2. `self.crossfader.set_overshoot(overshoot_pcm, spec)`
3. Create new decoder for the same segment of the NEW variant

In `fetch_next()`:
```rust
let chunk = decoder.next_chunk()?;
let chunk = self.crossfader.process(chunk); // no-op unless overshoot pending
```

On seek: `self.crossfader.reset();`

#### Data flow for overshoot segment

The old decoder reads from `SharedStream` which reads from `HlsSource`. At ABR
switch, `HlsSource` already has the old variant's next segment data (downloaded
before the switch was triggered). The old decoder can continue reading it.

The variant fence should NOT be activated until after the overshoot is complete.
Sequence:
1. `detect_format_change()` → sees new variant pending
2. Old decoder continues reading old variant data (fence not set yet)
3. Old decoder hits EOF at end of overshoot segment
4. Collect overshoot PCM
5. Now set variant fence + create new decoder + clear fence

### Code Cleanup

**Keep (essential):**
- Variant fence in `HlsSource` — race condition prevention
- Format change detection in `StreamAudioSource`
- Base offset tracking — Symphonia consistency

**Rewrite:**
- `Crossfader` struct — new clean API: `set_overshoot()` / `process()` / `reset()`
  (replaces rolling tail + arm/disarm state machine)

**Modify:**
- `StreamAudioSource::apply_format_change()` — overshoot + delayed fence activation

**Remove:**
- Rolling tail buffer, arm/disarm state machine from old Crossfader
- Any hardcoded skip/priming constants

## Simulator Tools

### Symphonia ABR switch simulator

Location: `crates/kithara-decode/examples/abr_switch_simulator.rs`

```
cargo run -p kithara-decode --example abr_switch_simulator -- /tmp/hls_analysis
```

Generates WAV files for A/B comparison:
- `{variant}_continuous.wav` — full continuous decode per variant
- `{from}_to_{to}_at{seg}_raw.wav` — raw hard cut (current behavior, clicks)
- `{from}_to_{to}_at{seg}_overlap.wav` — silence detection + xcorr + 20ms crossfade (no click)
- `{from}_to_{to}_at{seg}_apple.wav` — Apple implicit 2112/0 + xcorr + 20ms crossfade

### Apple AudioToolbox ABR switch simulator

Location: `crates/kithara-decode/examples/abr_switch_simulator_apple.rs`

```
cargo run -p kithara-decode --features apple --example abr_switch_simulator_apple -- /tmp/hls_analysis
```

Same analysis as Symphonia simulator but using Apple AudioToolbox decoder.
Confirmed identical results: same encoder delay values, same alignment offsets.

### PrimeInfo check tool

Location: `crates/kithara-decode/examples/prime_info_check.rs`

```
cargo run -p kithara-decode --features apple --example prime_info_check -- /tmp/hls_analysis
```

Queries `kAudioConverterPrimeInfo` from AudioToolbox to verify what the codec reports
as encoder delay. Currently returns zeros for all codecs (requires `elst` in container).

### AVPlayer HLS capture tool

Location: `/Users/litvinenko-pv/code/avplayer-test/capture.swift`

```
swiftc capture.swift -o avplayer-capture \
  -framework AVFoundation -framework CoreMedia -framework MediaToolbox
./avplayer-capture
```

Plays HLS stream via AVPlayer with `scalabilityToLosslessAudio`, forces AAC→FLAC
ABR switch, monitors access log. MTAudioProcessingTap attached but non-functional
for HLS on macOS 15. Confirms 0-stall transition via access log.

## References

- [Apple Appendix G: Audio Priming — Handling Encoder Delay in AAC](https://sosumi.ai/documentation/quicktime-file-format/appendix_g_audio_priming_handling_encoder_delay_in_aac) (full text with subpages)
- [Apple TN2258: AAC Encoder Delay and Synchronization](https://developer.apple.com/library/archive/technotes/tn2258/_index.html)
- [Apple: Background AAC Encoding](https://sosumi.ai/documentation/quicktime-file-format/background_aac_encoding) — transform architecture, decoder vs encoder delay
- [Apple: Using Track Structures for Explicit Delay](https://sosumi.ai/documentation/quicktime-file-format/using_track_structures_to_represent_encode_delay_explictly) — elst + sgpd + sbgp requirement
- [Apple: Historical Implicit Delay (2112)](https://sosumi.ai/documentation/quicktime-file-format/historical_solution_implicit_encoder_delay)
- [Apple: AVVariantPreferences (variantPreferences)](https://developer.apple.com/documentation/avfoundation/avplayeritem/3726149-variantpreferences) — `scalabilityToLosslessAudio` flag
- [Apple WWDC21: Explore HLS variants in AVFoundation](https://developer.apple.com/videos/play/wwdc2021/10143/) — variant selection, lossless audio downloads
- [Fraunhofer AAC Gapless Playback](https://www2.iis.fraunhofer.de/AAC/gapless.html)
- [Mozilla Bug 1703812: Edit list trimming](https://bugzilla.mozilla.org/show_bug.cgi?id=1703812)
- [AAC.js Wiki: Encoder Delay](https://github.com/uupaa/AAC.js/wiki/encoderdelay)
