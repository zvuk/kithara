#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

pub(crate) use kithara_integration_tests::user_sim::{actions, harness, scenarios};

mod tests;
