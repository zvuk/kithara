<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-key.svg)](https://crates.io/crates/kithara-key)
[![docs.rs](https://docs.rs/kithara-key/badge.svg)](https://docs.rs/kithara-key)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-key

Estimates one global musical key, a tonic and a mode, from decoded PCM. The
estimator folds each frame of the source to a pitch-class profile, sums the
profiles over an absolute frame grid and correlates the sum with the 24 key
profiles, so ranges may arrive in any order, with gaps or twice. Its state
round-trips through versioned bytes, and a restored estimator continues where
it stopped.

```rust,ignore
let mut estimator = KeyEstimator::new(sample_rate, &pools)?;
estimator.push(&pools, &interleaved, channels, first_frame)?;
estimator.set_end(total_frames)?;
if let Some(estimate) = estimator.estimate() {
    let key = estimate.key();
}
```

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-key) for the ownership contract.
