#![cfg(not(target_arch = "wasm32"))]

mod cases;
mod eq_programs;
mod filter_programs;
mod gain;
mod starvation;

pub(super) use cases::{
    ROOMY_CAPACITY, load_source, play_resource, play_source, playing_harness, render_blocks,
};
