<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-effects.svg)](https://crates.io/crates/kithara-effects)
[![docs.rs](https://docs.rs/kithara-effects/badge.svg)](https://docs.rs/kithara-effects)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-effects

Channel and master audio effects for Kithara. The crate owns the `AudioEffect`
contract, the effect chain and its end-of-stream drain, the band equaliser and
the DJ isolator, the stereo equaliser a deck runs, the peak limiter, and the
Firewheel node that puts the limiter on a session bus. It does not decode
audio, own player or session state, or reach the network.

## Usage

```rust
use kithara_effects::{
    GainDb,
    eq::{EqConfig, EqEffect, generate_log_spaced_bands},
};

fn boost_low_band<S>(config: &EqConfig<S>, sample_rate: u32, channels: u16) {
    let bands = generate_log_spaced_bands(5);
    let mut eq = EqEffect::new(config, bands, sample_rate, channels)
        .expect("pool region large enough for five bands");
    eq.set_gain(0, GainDb::from(3.0));
}
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>AudioEffect</code></td><td>Real-time contract every effect in a chain implements</td></tr>

<tr><td><code>EqEffect</code></td><td>Multi-band equaliser over decoded-audio chunks</td></tr>

<tr><td><code>IsolatorEq</code></td><td>DJ isolator built on crossover filters</td></tr>

<tr><td><code>PeakLimiter</code></td><td>Look-ahead peak limiter holding a ceiling</td></tr>

<tr><td><code>GainDb</code></td><td>Band gain in dB, clamped to a fixed range</td></tr>

<tr><td><code>StereoEq</code></td><td>Stereo equaliser crossing over between band layouts built off the audio thread</td></tr>

<tr><td><code>LimiterNode</code></td><td>Firewheel node putting the limiter on a session bus</td></tr>

</table>

## Features

<table>

<tr><th>Feature</th><th>Effect</th></tr>

<tr><td><code>mock</code></td><td>Exposes <code>AudioEffectMock</code> for consumers that fake a chain</td></tr>

<tr><td><code>usdt</code></td><td>Joins the workspace USDT probe lane</td></tr>

</table>

## Integration

`kithara-play` builds a per-track effect chain and the band layouts its deck
takes; `kithara-render` runs a `StereoEq` on every deck; `kithara-host` puts
`LimiterNode` on the session bus. Reusable DSP building
blocks stay private under `src/dsp/` until they become `kithara-dsp`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-effects) for detailed contracts, invariants, and internals.
