<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-waveform.svg)](https://crates.io/crates/kithara-waveform)
[![docs.rs](https://docs.rs/kithara-waveform/badge.svg)](https://docs.rs/kithara-waveform)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-waveform

The four-band stereo peak waveform: its value type, byte format, parameters,
and progressive analyzer. Without the `dsp` feature only the model
and codec are built, for apps that display or transfer a waveform without
analyzing audio. With `dsp`, the analyzer runs on `kithara-dsp` and can resume
a stopped pass exactly where it left off.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-waveform) for the ownership contract.

The analyzer keeps the first two channels separate (mono fills both sides) and splits each into four LR4 bands at 155, 1040 and 6957 Hz.
Each column holds `round(255 * sqrt(min(1, gain * peak)))` over 64 frames at 44.1/48 kHz, scaling with rate to stay within 2 ms.
