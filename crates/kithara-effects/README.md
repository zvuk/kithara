<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-effects.svg)](https://crates.io/crates/kithara-effects)
[![docs.rs](https://docs.rs/kithara-effects/badge.svg)](https://docs.rs/kithara-effects)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-effects

Deck and master audio effects for Kithara. The crate owns the band equaliser
and the DJ isolator, the stereo equaliser a deck runs, the deck feedback delay,
the peak limiter, and the Firewheel node that puts the limiter on a session bus. It does not decode
audio, own player or session state, or reach the network.

`filter::FilterEffect` is the deck filter knob: transparent at its center,
low-pass to the left and high-pass to the right, on the DSP owner's stereo
state-variable filter. `FilterSettings` fixes the cutoff ranges and Q; the knob
maps geometrically into them and the cutoff stays below 0.45 of the sample
rate. A manual knob glides; a program sets one knob position per frame without
another smoothing pass. Planar processing retains history across calls, and
reset clears history and completes the glide. The deck runs the filter after
FX units and before its output fader.

## Usage

```rust
use std::num::NonZeroU32;

use kithara_bufpool::HasPool;
use kithara_effects::{
    GainDb,
    eq::{EqConfig, EqLayout, StereoEq, generate_log_spaced_bands},
};

fn boost_low_band<S: HasPool<f32>>(config: &EqConfig<S>, sample_rate: NonZeroU32) -> StereoEq {
    let layout = EqLayout::new(config, &generate_log_spaced_bands(5), sample_rate)
        .expect("pool region large enough for five bands");
    let mut eq = StereoEq::with_layout(config, sample_rate, Box::new(layout));
    eq.set_gain(0, GainDb::from(3.0));
    eq
}
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>DeckEffect</code></td><td>Contract of the effect a deck FX unit holds: planar stereo wet signal, parameter changes, reset, tail exhaustion, sample-rate retune</td></tr>

<tr><td><code>FxParamChange</code></td><td>An effect parameter index and native-unit value, admitted by render before the batch applies</td></tr>

<tr><td><code>FxParamDescriptor</code></td><td>A checked native range, unit and optional linear/log mapping; no mapping declares integer steps</td></tr>

<tr><td><code>FeedbackDelay</code></td><td>Stereo feedback delay composing DSP history and a fixed high-pass; owns its comb policy and takes history from the pools</td></tr>

<tr><td><code>IsolatorEq</code></td><td>DJ isolator built on crossover filters</td></tr>

<tr><td><code>PeakLimiter</code></td><td>Linked peak limiter holding a ceiling; a lookahead delays its output by the frames it reports</td></tr>

<tr><td><code>GainDb</code></td><td>Band gain in dB, clamped to a fixed range</td></tr>

<tr><td><code>StereoEq</code></td><td>Stereo equaliser crossing over between band layouts built off the audio thread</td></tr>

<tr><td><code>LimiterNode</code></td><td>Firewheel node putting the limiter on a session bus</td></tr>

</table>

### Feedback delay

The effect owns its feedback comb, time-cross/pending policy and tail detector.
It composes the DSP delay line, fixed SVF and vector kernels; nothing after
construction allocates. Only the design cutoff saturates at the DSP limit:
the requested 20..=4000 Hz setting remains readable and returns at higher rates.

## Features

<table>

<tr><th>Feature</th><th>Effect</th></tr>

<tr><td><code>usdt</code></td><td>Joins the workspace USDT probe lane</td></tr>

</table>

## Integration

Effects belong to the deck: a track contributes only its slot gain and fader.
`kithara-play` builds the band layouts its deck takes; `kithara-render` runs a
`StereoEq` and the `DeckEffect` of each FX unit on every deck; `kithara-host` puts
`LimiterNode` on the session bus. The limiter's reconstruction, input history,
interval peaks and release recursion belong to `kithara-dsp`; effects owns its
tap design, ceiling, channel linking and per-frame sanitization policy.
`LimiterConfig` takes checked `Ceiling` (0.001..=1.0) and `ReleaseMs`
(1..=5000 ms) values; their defaults are 0.98 and 50 ms.

`DeckEffect::parameters` borrows descriptors prepared outside the audio thread.
They remain valid and unchanged across every supported sample-rate retune; the
effect reserves storage for that complete domain. Render checks manual changes
against these domains before any batch part applies, and `set` applies the
validated value without a second per-effect admission path.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-effects) for detailed contracts, invariants, and internals.
