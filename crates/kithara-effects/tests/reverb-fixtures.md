# Reverb synthetic fixtures

The fixtures are deterministic Rust arrays generated in `reverb_network.rs`. They
read no files, codecs, network or random source. They establish DSP and lifecycle
contracts, not musical listening acceptance.

The network reference is LSQAudioUnits commit
`1b7ac36a710332202e1b738e442ce47ae7b90093`, `Code/Reverb.hpp`,
`CombFilter.hpp:46` and `AllpassFilter.hpp:53`. No retained reference render is
used. The concrete topology is eight parallel damped combs and four serial
Freeverb-form diffusers per channel, with 13 reference-rate frames of right
spread. The independent oracle uses unbounded f64 histories and the declared
recurrences; it does not call the production network or delay storage.

| Axis | Fixture and threshold |
| --- | --- |
| Rates | 8,000 and 48,000 Hz for numerical contracts; parameter descriptors and retune also cover 96,000 Hz; the 1 Hz fixture pins one-frame section lengths |
| Rate changes | Retune preserves wet history, matches fresh construction after reset, refuses rates above the prepared maximum without changing PCM, and never revives an exhausted tail |
| Impulse | Left 1.0 at frame 0, right -0.5 at frame 7; the independent scalar network computes the expected wet PCM |
| Decay envelope | Decay RT60 0.1 and 10 seconds, no damping; RMS windows 1024..2048, 4096..5120 and 8192..9216 at 8,000 Hz; absolute RMS difference from the f64 oracle at most 2e-6 |
| Band energy | Both Decay and Damping extrema; Goertzel magnitude at 200 and 3,000 Hz at 8,000 Hz; difference from the oracle at most `1e-4 * expected + 1e-5`, and at maximum Decay, where the damped feedback dominates, maximum damping must reduce the actual 3 kHz magnitude |
| Scalar PCM | Both Damping extrema and Width 0, 0.37 and 1; absolute sample difference from the f64 oracle at most 1e-6 |
| Partition/reset | Uneven and empty spans, reset and retirement/re-entry; f32 bits must match exactly |
| Tail | Maximum Decay without damping, impulse and sustained DC excitation; every pre-retirement boundary step is at most 1e-6, and retirement occurs at the independently calculated bound |
| Prepared reserve | Current-rate output with a 96 kHz reserve matches current-rate-only construction bit for bit |
| Shapes/parameters | Refusal preserves history and output; admitted extrema stay finite; native domains reject nonfinite and out-of-range values |

Host acceptance uses the existing fixed constant WAV assets through public deck
FX commands. `deck_fx_reverb_wash_outlives_stop` compares the complete stopped
take with the same effect processing the dry take and requires an audible wet
tail after the dry source stops. The mix_tap case installs reverb, changes Decay
and Damping, clears it during playback, and is also selected by the RTSan lane.
