# Parity fixtures

Golden fixtures ported from `danigb/beat-this-rs` @ `089b509` (MIT), itself a
port of CPJKU `beat_this` (ISMIR 2024, MIT — code and weights).

- `golden_small.json` — beat/downbeat times in seconds produced by the Python
  reference `beat_this` v1.1.0 (`small1.ckpt`, minimal postprocessing, 50 fps)
  on `It Don't Mean A Thing - Kings of Swing.mp3`. Copied verbatim from
  `beat-this-rs/tests/fixtures/golden_small.json`.
- `it_dont_mean_a_thing_mono_22050.f32le` — the same track pre-decoded to raw
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

## Degara golden

`golden_degara.json` — beat times in seconds, the reference the
signal-processing backend is scored against. Recorded by
`record_degara_golden.py`, which carries the run instructions.

Provenance, so a later disagreement can be attributed:

- **Reference**: version 2.1-beta6-dev, PyPI wheel `cp314-macosx_15_0_arm64`.
  `record_degara_golden.py` names what it runs. AGPL-3.0 — run for its output,
  never read or carried across.
- **Algorithm**: `BeatTrackerDegara`, parameters `minTempo=40`,
  `maxTempo=208`, its defaults.
- **Input**: `beat_test_mono_22050.f32le`, the track the neural parity test
  uses, resampled 22 050 -> 44 100 Hz because the algorithm requires that rate.

292 beats over 155.69 s, median inter-beat interval 0.5225 s (114.8 BPM). The
neural backend tracks the same track at twice that: the two report different
metrical levels and are not expected to agree.

Parity criterion: F-measure >= 0.85 at the ±70 ms MIR window for beats. The
change's `design.md` records why that floor and not another.
