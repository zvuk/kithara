use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

static GEN_LOCK: Mutex<()> = Mutex::new(());

/// Generate (or retrieve from cache) a 1-second stereo silent ALAC
/// file at 44.1 kHz inside an M4A container. Returns the absolute path
/// to the cached fixture.
///
/// The cache lives under `CARGO_TARGET_TMPDIR` (set by cargo for
/// integration test binaries), falling back to `OUT_DIR`, then
/// `std::env::temp_dir()`. The function is idempotent: repeat calls
/// return the same path without regenerating.
///
/// # Panics
///
/// Panics when neither `afconvert` nor `ffmpeg` is available on `PATH`.
pub fn ensure_silence_1s_alac_m4a() -> PathBuf {
    let cache = cache_dir();
    let _ = std::fs::create_dir_all(&cache);
    let target = cache.join("silence_1s.m4a");

    let _guard = GEN_LOCK.lock().expect("alac fixture lock poisoned");
    if target.exists() && target.metadata().is_ok_and(|m| m.len() > 0) {
        return target;
    }

    let wav_src = locate_silence_wav();
    if try_afconvert(&wav_src, &target).is_ok() {
        return target;
    }
    if try_ffmpeg(&wav_src, &target).is_ok() {
        return target;
    }
    panic!(
        "ALAC fixture generation requires `afconvert` (macOS/iOS) or `ffmpeg` (other hosts) on PATH"
    );
}

fn cache_dir() -> PathBuf {
    if let Some(tmp) = std::env::var_os("CARGO_TARGET_TMPDIR") {
        return PathBuf::from(tmp).join("kithara-fixtures");
    }
    if let Some(out) = std::env::var_os("OUT_DIR") {
        return PathBuf::from(out).join("kithara-fixtures");
    }
    std::env::temp_dir().join("kithara-fixtures")
}

fn locate_silence_wav() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .parent()
        .map_or_else(|| manifest.clone(), Path::to_path_buf);
    let candidate = workspace_root.join("assets/silence_1s.wav");
    assert!(
        candidate.exists(),
        "expected workspace asset at {}; ALAC fixture generator can't seed",
        candidate.display()
    );
    candidate
}

fn try_afconvert(src: &Path, dst: &Path) -> std::io::Result<()> {
    let status = Command::new("afconvert")
        .args([
            "-f",
            "m4af",
            "-d",
            "alac",
            src.to_str().expect("utf8 src path"),
            dst.to_str().expect("utf8 dst path"),
        ])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "afconvert exited with {status}"
        )));
    }
    Ok(())
}

fn try_ffmpeg(src: &Path, dst: &Path) -> std::io::Result<()> {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            src.to_str().expect("utf8 src path"),
            "-c:a",
            "alac",
            dst.to_str().expect("utf8 dst path"),
        ])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "ffmpeg exited with {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "ios")),
        ignore = "requires afconvert or ffmpeg on host"
    )]
    fn alac_fixture_generates_or_loads_from_cache() {
        let p1 = ensure_silence_1s_alac_m4a();
        assert!(p1.exists(), "first call must materialise fixture");
        let size = p1.metadata().expect("metadata").len();
        assert!(size > 0, "fixture must be non-empty");

        let p2 = ensure_silence_1s_alac_m4a();
        assert_eq!(p1, p2, "second call must return cached path");
    }
}
