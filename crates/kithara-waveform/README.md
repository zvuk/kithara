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

## Usage

With `dsp`, the analyzer keeps the first two source channels apart; mono fills
both sides. Four LR4 bands follow the geometric boundaries of the four-band EQ
layout, approximately 155, 1040 and 6957 Hz.

A column holds each band's maximum absolute sample, mapped by
`round(255 * sqrt(min(1, gain * peak)))`. Heights are absolute: later coverage
and louder passages do not rescale existing columns. Digital silence is zero.

Columns span 64 source frames at 44.1/48 kHz, 128 at 96 kHz, and 256 at 192 kHz;
lower rates use shorter columns to stay within 2 ms. Each independent body
contains 48 columns, with a 16-column pre-roll after the first body. Range
order and checkpoint/resume produce identical quantized columns on one backend.
Padding after the source extent never contributes to a peak.

`Waveform` stores eight bytes per column, left bands followed by right bands.
`buckets()` decodes heights lazily. Version 2 accepts at most 524288 columns;
version 1 blobs are typed version errors and must be reanalysed. Snapshot
ceilings fold by maximum per side and band.
