//! Names the platform backends the enabled features select for the target,
//! so the sources gate on `apple_backend`, `android_backend` and
//! `symphonia_demuxer` instead of repeating the feature and target predicates.

use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(apple_backend, android_backend, symphonia_demuxer)");
    let target_os = env::var("CARGO_CFG_TARGET_OS");
    let apple = env::var_os("CARGO_FEATURE_APPLE").is_some()
        && matches!(target_os.as_deref(), Ok("macos" | "ios"));
    let android = env::var_os("CARGO_FEATURE_ANDROID").is_some()
        && matches!(target_os.as_deref(), Ok("android"));
    let symphonia = env::var_os("CARGO_FEATURE_SYMPHONIA").is_some();
    if apple {
        println!("cargo::rustc-cfg=apple_backend");
    }
    if android {
        println!("cargo::rustc-cfg=android_backend");
    }
    if symphonia || apple || android {
        println!("cargo::rustc-cfg=symphonia_demuxer");
    }
}
