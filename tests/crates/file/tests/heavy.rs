#![forbid(unsafe_code)]

use kithara_test_dylib as _;

#[path = "live_stress_real_mp3.rs"]
mod live_stress_real_mp3;
