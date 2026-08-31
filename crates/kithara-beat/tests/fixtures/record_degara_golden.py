"""Record the Degara golden fixtures.

Run for its output only: the tool is AGPL-3.0, so its numbers are data and its
source is never read. Commit the JSON; the crate stays MIT OR Apache-2.0.

    python3 -m venv venv && venv/bin/pip install --pre essentia numpy
    venv/bin/python record_degara_golden.py

Reads each pre-decoded mono fixture, resamples to the 44 100 Hz the algorithm
requires, and writes its goldens beside it: one over the whole file, and one
per scoring scenario over the same windows `tests/degara.rs` cuts, because a
detector called on windows must be compared to the reference called on the
same windows. The window constants below mirror `Pass` in that test.
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

WINDOW_SECONDS = 30
KEPT_SECONDS = 28
READY_SECONDS = 32

FIXTURES = [
    ("beat_test_mono_22050.f32le", "golden_degara.json"),
    ("track_excerpt_mono_22050.f32le", "golden_degara_track.json"),
]

WINDOWED = [
    ("beat_test_mono_22050.f32le", "golden_degara_windowed.json", 0),
    ("track_excerpt_mono_22050.f32le", "golden_degara_track_windowed.json", 0),
    ("track_excerpt_mono_22050.f32le", "golden_degara_track_windowed_from7.json", 7),
]


def record(source: str, golden: str) -> None:
    pcm = np.fromfile(HERE / source, dtype="<f4")
    resampled = es.Resample(
        inputSampleRate=SOURCE_RATE, outputSampleRate=ESSENTIA_RATE
    )(pcm)
    ticks = es.BeatTrackerDegara(minTempo=MIN_TEMPO, maxTempo=MAX_TEMPO)(resampled)

    (HERE / golden).write_text(
        json.dumps(
            {
                "beats": [round(float(t), 6) for t in ticks],
                "downbeats": [],
            },
            indent=2,
        )
        + "\n"
    )
    gaps = np.diff(ticks)
    print(
        f"{golden}: {len(ticks)} beats over {len(pcm) / SOURCE_RATE:.2f} s, "
        f"median {60 / float(np.median(gaps)):.2f} BPM"
    )


def ticks_of(slice_22050: np.ndarray) -> np.ndarray:
    resampled = es.Resample(
        inputSampleRate=SOURCE_RATE, outputSampleRate=ESSENTIA_RATE
    )(slice_22050)
    return es.BeatTrackerDegara(minTempo=MIN_TEMPO, maxTempo=MAX_TEMPO)(resampled)


def record_windowed(source: str, golden: str, from_seconds: int) -> None:
    pcm = np.fromfile(HERE / source, dtype="<f4")
    beats = []
    at = from_seconds * SOURCE_RATE
    while at < len(pcm):
        available = len(pcm) - at
        full = available >= READY_SECONDS * SOURCE_RATE
        end = at + WINDOW_SECONDS * SOURCE_RATE if full else len(pcm)
        kept = (KEPT_SECONDS * SOURCE_RATE if full else available) / SOURCE_RATE
        beats.extend(
            round(float(t) + at / SOURCE_RATE, 6)
            for t in ticks_of(pcm[at:end])
            if 0 <= t < kept
        )
        if not full:
            break
        at += KEPT_SECONDS * SOURCE_RATE

    (HERE / golden).write_text(
        json.dumps({"beats": beats, "downbeats": []}, indent=2) + "\n"
    )
    gaps = np.diff(beats)
    print(
        f"{golden}: {len(beats)} beats from {from_seconds} s, "
        f"median {60 / float(np.median(gaps)):.2f} BPM"
    )


def main() -> None:
    print(f"essentia {essentia.__version__}")
    for source, golden in FIXTURES:
        record(source, golden)
    for source, golden, from_seconds in WINDOWED:
        record_windowed(source, golden, from_seconds)


if __name__ == "__main__":
    main()
