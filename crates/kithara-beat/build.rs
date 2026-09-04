use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Where a fetched model lands, so a rebuild does not fetch it again.
const CACHE_ENV: &str = "KITHARA_BEAT_MODEL_CACHE";
const FULL_FILE: &str = "beat_this_full.onnx";
const FULL_URL: &str =
    "https://github.com/danigb/beat-this-rs/releases/download/model-large/beat_this.onnx";
const FULL_SHA256: &str = "5f810debe53459b559127fb55bbad40035bb47cc567b20e501670f968c770f02";
const INT8_FILE: &str = "beat_this_full_int8.onnx";

fn main() {
    println!("cargo::rerun-if-changed=models");
    println!("cargo::rerun-if-env-changed={CACHE_ENV}");

    let cache = cache_dir();
    if env::var_os("CARGO_FEATURE_EMBED_FULL_MODEL").is_some() {
        resolve(&cache, FULL_FILE, Some((FULL_URL, FULL_SHA256)));
    }
    if env::var_os("CARGO_FEATURE_EMBED_FULL_INT8_MODEL").is_some() {
        resolve(&cache, INT8_FILE, None);
    }
}

fn cache_dir() -> PathBuf {
    env::var_os(CACHE_ENV).map_or_else(
        || env::temp_dir().join("kithara-beat-models"),
        PathBuf::from,
    )
}

/// Puts the model in the cache and names it to the compiler. A model the
/// release publishes is fetched and checked; one that is quantized locally can
/// only be reported missing.
fn resolve(cache: &Path, file: &str, source: Option<(&str, &str)>) {
    let path = cache.join(file);
    println!("cargo::rerun-if-changed={}", path.display());
    if !path.exists() {
        let Some((url, sha256)) = source else {
            println!(
                "cargo::error={file} is missing from {}; quantize it with \
                 `uv run --with onnx --with onnxruntime \
                 https://raw.githubusercontent.com/danigb/beat-this-rs/main/scripts/quantize_int8.py \
                 --input {}/{FULL_FILE} --output {}`",
                cache.display(),
                cache.display(),
                path.display()
            );
            return;
        };
        if !fetch(cache, &path, url) {
            return;
        }
        if !verify(&path, sha256) {
            let _ = fs::remove_file(&path);
            return;
        }
    }
    println!("cargo::rustc-env=KITHARA_BEAT_MODEL={}", path.display());
}

fn fetch(cache: &Path, path: &Path, url: &str) -> bool {
    if let Err(err) = fs::create_dir_all(cache) {
        println!("cargo::error=cannot create {}: {err}", cache.display());
        return false;
    }
    let partial = path.with_extension("onnx.part");
    let status = Command::new("curl")
        .args(["-fL", "--retry", "3", "-o"])
        .arg(&partial)
        .arg(url)
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            println!("cargo::error=curl {url} exited with {status}");
            return false;
        }
        Err(err) => {
            println!("cargo::error=cannot run curl to fetch {url}: {err}");
            return false;
        }
    }
    if let Err(err) = fs::rename(&partial, path) {
        println!("cargo::error=cannot place {}: {err}", path.display());
        return false;
    }
    true
}

fn verify(path: &Path, expected: &str) -> bool {
    let tool = if Command::new("sha256sum").arg("--version").output().is_ok() {
        "sha256sum"
    } else {
        "shasum"
    };
    let mut command = Command::new(tool);
    if tool == "shasum" {
        command.args(["-a", "256"]);
    }
    let output = match command.arg(path).output() {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            println!(
                "cargo::error={tool} failed on {}: {output:?}",
                path.display()
            );
            return false;
        }
        Err(err) => {
            println!("cargo::error=cannot run {tool}: {err}");
            return false;
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(actual) = text.split_whitespace().next() else {
        println!("cargo::error={tool} printed nothing for {}", path.display());
        return false;
    };
    if actual != expected {
        println!(
            "cargo::error={} hashes to {actual}, expected {expected}",
            path.display()
        );
        return false;
    }
    true
}
