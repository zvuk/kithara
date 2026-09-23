//! Names the Apple and reqwest-family backends the enabled features select
//! for the target, so the sources gate on `apple_backend` and
//! `reqwest_backend` instead of repeating the feature and target predicates.

use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(apple_backend, reqwest_backend)");
    let apple = env::var_os("CARGO_FEATURE_CLIENT_APPLE").is_some()
        && matches!(
            env::var("CARGO_CFG_TARGET_OS").as_deref(),
            Ok("macos" | "ios")
        );
    let host = env::var_os("CARGO_FEATURE_CLIENT_HOST").is_some();
    if apple {
        println!("cargo::rustc-cfg=apple_backend");
    }
    if !apple && !host {
        println!("cargo::rustc-cfg=reqwest_backend");
    }
}
