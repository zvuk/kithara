#![cfg(not(target_arch = "wasm32"))]

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;

mod behaviour;
mod parity;
mod recipe;
