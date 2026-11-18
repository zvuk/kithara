"""Record the KeyExtractor golden fixtures.

The reference is AGPL-3.0. The crate reimplements its published algorithm and
constants in Rust and copies none of its code; this script runs the reference
only to produce numbers, the JSON it writes is data, and the crate stays
MIT OR Apache-2.0.

    python3 -m venv venv && venv/bin/pip install --pre essentia==2.1b6.dev1438 numpy
    venv/bin/python record_key_golden.py

Each fixture is a recipe, not PCM: chords of triads, each note a sum of
harmonic partials. `render` is the formula `tests/key/recipe.rs` repeats, so both
sides build the same samples. The script writes every recipe with the key,
scale and strength that `KeyExtractor` at its defaults states for it. The
silence recipe carries no answer: the reference adds unseeded noise to silent
frames, so its key is not reproducible.
"""

import importlib.metadata
import json
import math
import pathlib

import numpy as np
import essentia.standard as es

HERE = pathlib.Path(__file__).parent
ESSENTIA = "2.1b6.dev1438"
SAMPLE_RATE = 44100
GAIN = 0.1
PARTIALS = [1.0, 0.5, 0.33, 0.25]
CHORD_SECONDS = 3.0

# (name, tonic as a MIDI note, minor)
PROGRESSIONS = [
    ("c_major", 60, False),
    ("g_major", 55, False),
    ("e_flat_major", 51, False),
    ("f_sharp_major", 54, False),
    ("a_minor", 57, True),
    ("e_minor", 52, True),
    ("c_sharp_minor", 49, True),
    ("f_minor", 53, True),
]


def triad(root: int, minor: bool) -> list[int]:
    return [root, root + (3 if minor else 4), root + 7]


def progression(tonic: int, minor: bool) -> list[dict]:
    """i - iv - v - i, or I - IV - V - I."""
    roots = [tonic, tonic + 5, tonic + 7, tonic]
    return [
        {"seconds": CHORD_SECONDS, "notes": triad(root, minor)} for root in roots
    ]


def render(recipe: dict, sample_rate: int) -> np.ndarray:
    """x[n] = gain * sum over notes, then partials h, of
    a_h * sin(2 pi f (h + 1) n / rate), f = 440 * 2^((note - 69) / 12),
    in f64 with n the absolute sample index, then rounded to f32."""
    total = sum(round(chord["seconds"] * sample_rate) for chord in recipe["chords"])
    out = np.zeros(total, dtype=np.float64)
    at = 0
    for chord in recipe["chords"]:
        frames = round(chord["seconds"] * sample_rate)
        n = np.arange(at, at + frames, dtype=np.float64)
        for note in chord["notes"]:
            freq = 440.0 * 2.0 ** ((note - 69) / 12.0)
            for h, amplitude in enumerate(recipe["partials"]):
                out[at : at + frames] += amplitude * np.sin(
                    2.0 * math.pi * freq * (h + 1) * n / sample_rate
                )
        at += frames
    return (recipe["gain"] * out).astype(np.float32)


def main() -> None:
    installed = importlib.metadata.version("essentia")
    assert installed == ESSENTIA, f"essentia {installed} != pinned {ESSENTIA}"

    fixtures = []
    for name, tonic, minor in PROGRESSIONS:
        recipe = {
            "name": name,
            "gain": GAIN,
            "partials": PARTIALS,
            "chords": progression(tonic, minor),
        }
        key, scale, strength = es.KeyExtractor()(render(recipe, SAMPLE_RATE))
        # The f32 strength in its shortest decimal form, which JSON formatters keep.
        strength = float(str(np.float32(strength)))
        recipe["golden"] = {"key": key, "scale": scale, "strength": strength}
        fixtures.append(recipe)
        print(f"{name}: {key} {scale} {strength:.6f}")

    fixtures.append(
        {
            "name": "silence",
            "gain": GAIN,
            "partials": PARTIALS,
            "chords": [{"seconds": 4 * CHORD_SECONDS, "notes": []}],
            "golden": None,
        }
    )

    document = {
        "essentia": ESSENTIA,
        "sample_rate": SAMPLE_RATE,
        "fixtures": fixtures,
    }
    (HERE / "golden_key.json").write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
