<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-link.svg)](https://crates.io/crates/kithara-link)
[![docs.rs](https://docs.rs/kithara-link/badge.svg)](https://docs.rs/kithara-link)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-link

Track and Host decorators that synchronize decks to one Host tempo trajectory.
Compose `LinkedFactory` with `Queue`, then register it with `LinkedHost`.
Pure placement and tempo mathematics belong to `kithara-sync` and are re-exported
from this crate; receipt-driven custody remains in the decorators.
