"""Record the Degara golden fixture from a real Essentia run.

Reference output only: Essentia is AGPL-3.0, so its numbers are data and its
source is never read. Run it, commit the JSON, keep the crate MIT OR Apache-2.0.

    python3 -m venv venv && venv/bin/pip install --pre essentia numpy
    venv/bin/python record_degara_golden.py

Reads `beat_test_mono_22050.f32le`, resamples to the 44 100 Hz the algorithm
requires, and writes `golden_degara.json`.
"""

import json
import pathlib

import numpy as np
import essentia
import essentia.standard as es

HERE = pathlib.Path(__file__).parent
SOURCE_RATE = 22050
ESSENTIA_RATE = 44100
MIN_TEMPO = 40
MAX_TEMPO = 208


def main() -> None:
    pcm = np.fromfile(HERE / "beat_test_mono_22050.f32le", dtype="<f4")
    resampled = es.Resample(
        inputSampleRate=SOURCE_RATE, outputSampleRate=ESSENTIA_RATE
    )(pcm)
    ticks = es.BeatTrackerDegara(minTempo=MIN_TEMPO, maxTempo=MAX_TEMPO)(resampled)

    (HERE / "golden_degara.json").write_text(
        json.dumps(
            {
                "beats": [round(float(t), 6) for t in ticks],
                "downbeats": [],
            },
            indent=2,
        )
        + "\n"
    )
    print(f"essentia {essentia.__version__}")
    print(f"{len(ticks)} beats over {len(pcm) / SOURCE_RATE:.2f} s")


if __name__ == "__main__":
    main()
