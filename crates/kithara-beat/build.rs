use std::path::Path;

/// The full models are too large for git. Name the one a feature asked for
/// before `include_bytes!` fails on a path with no file behind it.
fn main() {
    println!("cargo::rerun-if-changed=models");
    let wanted = [
        ("CARGO_FEATURE_EMBED_FULL_MODEL", "beat_this_full.onnx"),
        (
            "CARGO_FEATURE_EMBED_FULL_INT8_MODEL",
            "beat_this_full_int8.onnx",
        ),
    ];
    for (feature, file) in wanted {
        if std::env::var_os(feature).is_none() {
            continue;
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("models")
            .join(file);
        if !path.exists() {
            println!(
                "cargo::error=models/{file} is missing; run crates/kithara-beat/scripts/fetch-beat-models.sh to fetch it"
            );
        }
    }
}
