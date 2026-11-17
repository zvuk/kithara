# Key goldens

`golden_key.json` holds recipes, not PCM, each with the answer of the reference.
`record_key_golden.py` writes it, and `tests/key/recipe.rs` renders the same recipes.

- **Reference**: Essentia `2.1b6.dev1438` (`v2.1_beta5-1438-g8dbdc073`), macOS arm64
  wheel. It is AGPL-3.0 and runs only to produce the numbers.
- **Algorithm**: KeyExtractor at its defaults, at 44 100 Hz.
- **Recipes**: four major and four minor progressions, each I-IV-V-I or i-iv-v-i in
  triads, 3 s per chord. Every note is the sum of four harmonic partials. The silence
  recipe carries no answer, because the reference adds unseeded noise to silent frames.

Parity criterion: the key matches exactly and the strength is within 1e-3. Record the
goldens again only for an intended change of the pinned version:

```
python3 -m venv venv && venv/bin/pip install --pre essentia==2.1b6.dev1438 numpy
venv/bin/python record_key_golden.py
```

A second run writes byte-identical JSON.
