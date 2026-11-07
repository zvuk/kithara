#![cfg(not(target_arch = "wasm32"))]

mod cases;
mod gain;
mod starvation;

pub(super) use cases::{
    ROOMY_CAPACITY, play_resource, play_source, playing_harness, render_blocks,
};
