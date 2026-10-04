use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=MONKEYS_AUDIO_DIR");
    if env::var_os("CARGO_FEATURE_MONKEYS_AUDIO").is_none()
        || env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32")
    {
        return;
    }
    if let Some(prefix) = env::var_os("MONKEYS_AUDIO_DIR") {
        println!(
            "cargo:rustc-link-search=native={}",
            PathBuf::from(prefix).join("lib").display()
        );
    } else if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let prefix = Command::new("brew")
            .args(["--prefix", "mac"])
            .output()
            .expect("Monkey's Audio SDK prefix");
        assert!(prefix.status.success(), "install the Monkey's Audio SDK");
        println!(
            "cargo:rustc-link-search=native={}/lib",
            String::from_utf8_lossy(&prefix.stdout).trim()
        );
    }
    println!("cargo:rustc-link-lib=MAC");
}
