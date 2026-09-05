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

## Degara goldens

Beat times in seconds, the reference the signal-processing backend is scored
against. All are recorded by `record_degara_golden.py`, which carries the run
instructions.

Whole-file references, one run over the entire fixture:

- `golden_degara.json` over `beat_test_mono_22050.f32le`: 292 beats over
  155.69 s, median 114.84 BPM. The track the neural parity test also uses.
- `golden_degara_track.json` over `track_excerpt_mono_22050.f32le`: 86 beats
  over 45.00 s, median 109.96 BPM. The first 45 seconds of `assets/track.mp3`,
  where the reference holds one steady level and reading a submultiple is the
  failure to catch. An excerpt rather than the whole track because a decoded
  fixture costs four times what the committed mp3 does, and 45 s covers the
  region that discriminates.

Windowed references, the ones `tests/degara.rs` scores against: the reference
run over the same windows that test cuts (30 s per call, the front 28 s kept,
a window only cut to length while 32 s is still ahead). A detector called on
windows must be compared to the reference called on the same windows: a
30-second window does not determine the phase a whole-file run settles on, so
the whole-file references above, sliced into these windows, agree with the
windowed runs at only F = 0.64-0.84 - regime noise no implementation can
close.

- `golden_degara_windowed.json`: the click fixture from 0 s, 289 beats.
- `golden_degara_track_windowed.json`: the track excerpt from 0 s, 87 beats.
- `golden_degara_track_windowed_from7.json`: the excerpt from 7 s, 74 beats,
  cutting the same music at another alignment.

Provenance, so a later disagreement can be attributed:

- **Reference**: version 2.1-beta6-dev, a PyPI `cp314` macOS arm64 wheel
  (whole-file goldens: build `macosx_15_0`; windowed goldens: build
  `2.1b6.dev1438`, `macosx_26_0`, whose whole-file run reproduces both
  committed whole-file goldens byte for byte). `record_degara_golden.py`
  names what it runs. AGPL-3.0 — run for its output, never carried across.
- **Algorithm**: `BeatTrackerDegara`, parameters `minTempo=40`,
  `maxTempo=208`, its defaults.
- **Input**: the pre-decoded mono fixtures above, resampled 22 050 -> 44 100 Hz
  because the algorithm requires that rate; windowed goldens slice at
  22 050 Hz first, the way the test slices, and resample each window.

The neural backend tracks the first track at twice the reference's level: the
two backends report different metrical levels and are not expected to agree.

Parity criterion: F-measure >= 0.85 at the ±70 ms MIR window for beats, and a
grid within 5% of the reference's tempo. The change's `design.md` records why
that floor and not another.
