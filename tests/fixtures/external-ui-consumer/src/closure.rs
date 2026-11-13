use std::{collections::BTreeSet, fs, process::Command};

fn native_apple_masonry_has_no_product_audio_union() {
    let output = Command::new("cargo")
        .arg("tree")
        .current_dir(std::env::temp_dir())
        .arg("--manifest-path")
        .arg(format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR")))
        .args([
            "--offline",
            "--edges",
            "normal,build",
            "--target",
            "aarch64-apple-darwin",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--no-default-features",
            "--features",
            "masonry",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::create_dir_all("target/acceptance").unwrap();
    fs::write(
        "target/acceptance/normal-build-apple-masonry.txt",
        &output.stdout,
    )
    .unwrap();
    let graph = String::from_utf8(output.stdout).unwrap();
    let forbidden: BTreeSet<_> = graph
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| {
            name.starts_with("firewheel") || *name == "cpal" || *name == "kithara-workspace-hack"
        })
        .collect();
    assert!(
        forbidden.is_empty(),
        "UI normal/build graph contains product union: {forbidden:?}"
    );
}

fn main() {
    native_apple_masonry_has_no_product_audio_union();
}
