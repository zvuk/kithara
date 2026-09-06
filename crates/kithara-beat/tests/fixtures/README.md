# Parity fixtures

Golden fixtures ported from `danigb/beat-this-rs` @ `089b509` (MIT), itself a
port of CPJKU `beat_this` (ISMIR 2024, MIT — code and weights).

- `golden_small.json` — beat/downbeat times in seconds produced by the Python
  reference `beat_this` v1.1.0 (`small1.ckpt`, minimal postprocessing, 50 fps)
  on `It Don't Mean A Thing - Kings of Swing.mp3`. Copied verbatim from
  `beat-this-rs/tests/fixtures/golden_small.json`.
- `beat_test_mono_22050.f32le` — the same track pre-decoded to raw
  mono f32 little-endian PCM at 22 050 Hz (3 432 959 samples, 155.69 s).
  Produced offline from `beat-this-rs/test_files/It Don't Mean A Thing - Kings of Swing.mp3` via that crate's own `load_audio` path (symphonia 0.6
  decode → channel-average downmix → rubato 3.0 sinc resample, `sinc_len` 256,
  Blackman-Harris2) — the exact input its parity suite fed the pipeline.
  Pre-decoding keeps `kithara-beat` free of decoder/resampler dependencies:
  the crate contract starts at mono f32 22 050 Hz.

Parity criterion: F-measure >= 0.99 at the standard ±70 ms MIR window for both
beats and downbeats. The small structural model has a few logit peaks right at
the `> 0` threshold where rten's float output differs from torch by an epsilon,
so exact F = 1.0 is not guaranteed (it is for the full FP32 model, which proves
the shared pipeline stages exact).

## Degara goldens

Beat times in seconds, the reference the signal-processing backend is scored against, all
recorded by `record_degara_golden.py`.

| golden | fixture | from | beats | median |
|---|---|---|---|---|
| `golden_degara.json` | `beat_test_mono_22050.f32le` | whole file | 292 | 114.84 BPM |
| `golden_degara_track.json` | `track_excerpt_mono_22050.f32le` | whole file | 86 | 109.96 BPM |
| `golden_degara_windowed.json` | `beat_test_mono_22050.f32le` | windowed, 0 s | 289 | |
| `golden_degara_track_windowed.json` | `track_excerpt_mono_22050.f32le` | windowed, 0 s | 87 | |
| `golden_degara_track_windowed_from7.json` | `track_excerpt_mono_22050.f32le` | windowed, 7 s | 74 | |

`track_excerpt_mono_22050.f32le` is the first 45 s of `assets/track.mp3`, where the reference
holds one steady level and reading a submultiple is the failure to catch.

`tests/degara.rs` scores against the windowed goldens, recorded the way that test cuts: 30 s per
call, the front 28 s kept, a window only cut to length while 32 s is still ahead. Sliced into
those windows, the whole-file goldens agree with the windowed runs at only F = 0.64-0.84.

- **Reference**: Essentia 2.1-beta6-dev, macOS arm64 wheel. AGPL-3.0 - run for its output,
  never carried across.
- **Algorithm**: Essentia's BeatTrackerDegara at its defaults, minTempo 40, maxTempo 208.
- **Input**: the mono fixtures above, resampled 22 050 -> 44 100 Hz because the algorithm
  requires that rate; windowed goldens slice at 22 050 Hz first, the way the test slices.

Parity criterion: F-measure >= 0.85 at the +-70 ms MIR window, and a grid within 5% of the
reference's tempo.
