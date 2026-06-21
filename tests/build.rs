//! Emits `KITHARA_FIXTURE_BUILD`: a fingerprint of the fixture-encoding code and
//! its dependency lockfile, baked into every test binary of this package.
//!
//! The L2 fixture cache (see `src/fixture_cache.rs`) namespaces its default
//! directory by this value, so:
//! - all test binaries of one build (`suite_stress`, `suite_heavy`, …) share the
//!   same cache dir — an AAC fixture encoded by one binary is reused by every
//!   other binary and by repeated runs of the same build;
//! - a change to the encoder, the fixture server, or any dependency version
//!   yields a fresh namespace, so a stale cache can never serve outdated bytes.

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::Path,
};

/// Hash every `.rs` file under `dir` (path + contents) and register each for
/// change-tracking so the fingerprint refreshes whenever encoding code changes.
fn hash_rs_tree(dir: &Path, hasher: &mut DefaultHasher) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            hash_rs_tree(&path, hasher);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            if let Ok(bytes) = fs::read(&path) {
                path.to_string_lossy().hash(hasher);
                bytes.hash(hasher);
            }
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn main() {
    let mut hasher = DefaultHasher::new();

    // Hash ONLY the spec→bytes transformation code: the encoders, the fMP4
    // muxer, the packaged-variant encode glue, and the signal encode route.
    // Spec changes flow into per-entry cache KEYS, and server delivery code
    // (routes/behavior, throttling, playlists) never changes encoded bytes —
    // hashing all of `src/native` forced a cold cache (and per-test ffmpeg
    // re-encodes blowing test budgets) on every test-server edit.
    for dir in ["../crates/kithara-encode/src", "src/native/fmp4"] {
        println!("cargo:rerun-if-changed={dir}");
        hash_rs_tree(Path::new(dir), &mut hasher);
    }
    // The encode glue, the PCM signal generator, and the dependency lockfile
    // (ffmpeg-sys / encoder crate versions) also determine the encoded bytes.
    for file in [
        "src/native/hls_stream.rs",
        "src/native/routes/signal.rs",
        "src/signal_pcm.rs",
        "../Cargo.lock",
    ] {
        if let Ok(bytes) = fs::read(file) {
            bytes.hash(&mut hasher);
        }
        println!("cargo:rerun-if-changed={file}");
    }
    println!("cargo:rerun-if-changed=build.rs");

    println!(
        "cargo:rustc-env=KITHARA_FIXTURE_BUILD={:016x}",
        hasher.finish()
    );
}
