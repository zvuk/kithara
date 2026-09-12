use std::{
    collections::BTreeMap,
    env, fs,
    io::Read as _,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::FileLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::logged;
use crate::{
    android::ndk_root,
    child,
    ci::{CiPins, PINS_PATH},
};

#[derive(Deserialize, Serialize)]
struct Provenance {
    inputs: BTreeMap<String, String>,
    files: BTreeMap<PathBuf, String>,
}

pub(super) fn prepare(
    root: &Path,
    target: &str,
    compiler: &Path,
    evidence: &Path,
    cancel: &child::Cancel,
) -> Result<PathBuf> {
    let pins = CiPins::load(&root.join(PINS_PATH))?;
    let ndk = ndk_root()?;
    let version = child::output(
        Command::new(compiler).arg("--version"),
        Some(cancel),
        Duration::from_secs(10),
    )?;
    if !version.status.success() {
        bail!("reading Android compiler version failed");
    }
    let inputs = BTreeMap::from([
        ("target".into(), target.into()),
        ("compiler".into(), compiler.display().to_string()),
        (
            "compiler_version".into(),
            String::from_utf8(version.stdout)?,
        ),
        (
            "ndk".into(),
            fs::read_to_string(ndk.join("source.properties"))?,
        ),
        ("ffmpeg_version".into(), pins.ffmpeg_source_version.clone()),
        ("ffmpeg_sha256".into(), pins.ffmpeg_source_sha256.clone()),
        ("lame_version".into(), pins.lame_source_version.clone()),
        ("lame_sha256".into(), pins.lame_source_sha256.clone()),
        (
            "recipe_sha256".into(),
            digest(&root.join("xtask/src/android/native/deps.rs"))?,
        ),
    ]);
    let key = hex::encode(Sha256::digest(serde_json::to_vec(&inputs)?));
    let target_dir =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), |path| root.join(path));
    let cache = target_dir.join("android-native-deps").join(key);
    fs::create_dir_all(&cache)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("build.lock"))?;
    let _lock = FileLock::try_exclusive(lock)
        .context("Android dependency cache is in use by another build")?;
    let result: Result<PathBuf> = (|| {
        let prefix = cache.join("install");
        let receipt = cache.join("provenance.json");
        if receipt.exists() {
            let recorded: Provenance = serde_json::from_slice(&fs::read(&receipt)?)?;
            if recorded.inputs != inputs || recorded.files != inventory(&prefix)? {
                bail!(
                    "Android dependency provenance mismatch: {}",
                    receipt.display()
                );
            }
            return Ok(prefix);
        }
        let sources = cache.join("sources");
        for directory in [&prefix, &sources] {
            if directory.exists() {
                fs::remove_dir_all(directory)?;
            }
            fs::create_dir_all(directory)?;
        }
        let lame = archive(
            &cache,
            &sources,
            "lame",
            &format!(
                "https://downloads.sourceforge.net/project/lame/lame/{0}/lame-{0}.tar.gz",
                pins.lame_source_version
            ),
            &pins.lame_source_sha256,
            cancel,
        )?;
        let ffmpeg = archive(
            &cache,
            &sources,
            "ffmpeg",
            &format!(
                "https://ffmpeg.org/releases/ffmpeg-{}.tar.xz",
                pins.ffmpeg_source_version
            ),
            &pins.ffmpeg_source_sha256,
            cancel,
        )?;
        build_lame(&lame, &prefix, target, compiler, cancel)?;
        build_ffmpeg(&ffmpeg, &prefix, target, compiler, cancel)?;
        let files = inventory(&prefix)?;
        for library in [
            "avcodec",
            "avformat",
            "avfilter",
            "avdevice",
            "avutil",
            "swresample",
            "swscale",
            "mp3lame",
        ] {
            if !files.contains_key(&PathBuf::from(format!("lib/lib{library}.a"))) {
                bail!("Android dependency install is missing lib{library}.a");
            }
        }
        fs::write(
            &receipt,
            serde_json::to_vec_pretty(&Provenance { inputs, files })?,
        )?;
        Ok(prefix)
    })();
    let preserved = preserve_evidence(&cache, evidence);
    let prefix = result?;
    preserved?;
    Ok(prefix)
}

fn preserve_evidence(cache: &Path, evidence: &Path) -> Result<()> {
    let destination = evidence.join("dependencies");
    let mut paths = vec![PathBuf::from("provenance.json")];
    for name in ["lame", "ffmpeg"] {
        for operation in ["download", "extract"] {
            paths.push(format!("{name}-{operation}.stdout.log").into());
            paths.push(format!("{name}-{operation}.log").into());
        }
        for operation in ["configure", "make", "install"] {
            for stream in ["stdout", "stderr"] {
                paths.push(format!("sources/{name}/{operation}.{stream}.log").into());
            }
        }
    }
    paths.push("sources/lame/config.log".into());
    paths.push("sources/ffmpeg/ffbuild/config.log".into());
    for path in paths {
        let source = cache.join(&path);
        if source.is_file() {
            let output = destination.join(&path);
            fs::create_dir_all(output.parent().context("dependency evidence parent")?)?;
            fs::copy(source, output)?;
        }
    }
    Ok(())
}

fn archive(
    cache: &Path,
    sources: &Path,
    name: &str,
    url: &str,
    expected: &str,
    cancel: &child::Cancel,
) -> Result<PathBuf> {
    let archive = cache.join(format!("{name}.tar"));
    if !archive.exists() {
        let pending = cache.join(format!("{name}.download"));
        logged(
            Command::new("curl")
                .args([
                    "--fail",
                    "--location",
                    "--connect-timeout",
                    "30",
                    "--max-time",
                    "600",
                    "--output",
                ])
                .arg(&pending)
                .arg(url),
            &cache.join(format!("{name}-download.stdout.log")),
            &cache.join(format!("{name}-download.log")),
            cancel,
        )?;
        verify_digest(&pending, expected)?;
        fs::rename(pending, &archive)?;
    }
    verify_digest(&archive, expected)?;
    let directory = sources.join(name);
    fs::create_dir_all(&directory)?;
    logged(
        Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&directory)
            .arg("--strip-components=1"),
        &cache.join(format!("{name}-extract.stdout.log")),
        &cache.join(format!("{name}-extract.log")),
        cancel,
    )?;
    Ok(directory)
}

fn build_lame(
    source: &Path,
    prefix: &Path,
    target: &str,
    compiler: &Path,
    cancel: &child::Cancel,
) -> Result<()> {
    let tools = compiler
        .parent()
        .context("Android compiler has no directory")?;
    let mut configure = Command::new(source.join("configure"));
    configure
        .current_dir(source)
        .arg(format!("--host={target}"))
        .arg(format!("--prefix={}", prefix.display()))
        .args([
            "--enable-static",
            "--disable-shared",
            "--with-pic",
            "--disable-frontend",
            "--disable-decoder",
            "--disable-nasm",
        ])
        .env("CC", compiler)
        .env("AR", tools.join("llvm-ar"))
        .env("RANLIB", tools.join("llvm-ranlib"))
        .env("NM", tools.join("llvm-nm"))
        .env("STRIP", tools.join("llvm-strip"))
        .env("CFLAGS", "-O2 -fPIC")
        .env("CPPFLAGS", "")
        .env("LDFLAGS", "")
        .env("LIBS", "");
    build(source, &mut configure, cancel)
}

fn build_ffmpeg(
    source: &Path,
    prefix: &Path,
    target: &str,
    compiler: &Path,
    cancel: &child::Cancel,
) -> Result<()> {
    let tools = compiler
        .parent()
        .context("Android compiler has no directory")?;
    let arch = match target {
        "aarch64-linux-android" => "aarch64",
        "x86_64-linux-android" => "x86_64",
        _ => bail!("Android FFmpeg target {target} is not supported"),
    };
    let mut configure = Command::new(source.join("configure"));
    configure
        .current_dir(source)
        .arg(format!("--prefix={}", prefix.display()))
        .arg(format!("--arch={arch}"))
        .arg(format!("--cc={}", compiler.display()))
        .arg(format!(
            "--cxx={}",
            tools
                .join(format!(
                    "{}++",
                    compiler
                        .file_name()
                        .context("compiler filename")?
                        .to_string_lossy()
                ))
                .display()
        ))
        .args([
            "--target-os=android",
            "--enable-cross-compile",
            "--enable-static",
            "--disable-shared",
            "--enable-pic",
            "--disable-autodetect",
            "--disable-programs",
            "--disable-doc",
            "--enable-libmp3lame",
            "--pkg-config-flags=--static",
        ])
        .arg(format!(
            "--extra-cflags=-I{}",
            prefix.join("include").display()
        ))
        .arg(format!(
            "--extra-ldflags=-L{}",
            prefix.join("lib").display()
        ))
        .env("PKG_CONFIG_PATH", "")
        .env("PKG_CONFIG_LIBDIR", prefix.join("lib/pkgconfig"))
        .env("PKG_CONFIG_SYSROOT_DIR", "/")
        .env("CFLAGS", "")
        .env("CPPFLAGS", "")
        .env("LDFLAGS", "")
        .env("LIBS", "");
    for (flag, tool) in [
        ("ar", "llvm-ar"),
        ("ranlib", "llvm-ranlib"),
        ("nm", "llvm-nm"),
        ("strip", "llvm-strip"),
    ] {
        configure.arg(format!("--{flag}={}", tools.join(tool).display()));
    }
    if arch == "x86_64" {
        configure.arg("--disable-x86asm");
    }
    build(source, &mut configure, cancel)
}

fn build(source: &Path, configure: &mut Command, cancel: &child::Cancel) -> Result<()> {
    logged(
        configure,
        &source.join("configure.stdout.log"),
        &source.join("configure.stderr.log"),
        cancel,
    )?;
    let jobs = std::thread::available_parallelism()?.get();
    logged(
        Command::new("make")
            .current_dir(source)
            .arg(format!("-j{jobs}")),
        &source.join("make.stdout.log"),
        &source.join("make.stderr.log"),
        cancel,
    )?;
    logged(
        Command::new("make").current_dir(source).arg("install"),
        &source.join("install.stdout.log"),
        &source.join("install.stderr.log"),
        cancel,
    )
}

fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, String>) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                collect(root, &path, files)?;
            } else if kind.is_file() {
                files.insert(path.strip_prefix(root)?.to_owned(), digest(&path)?);
            } else {
                bail!(
                    "Android dependency install has a non-regular entry: {}",
                    path.display()
                );
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    collect(root, root, &mut files)?;
    Ok(files)
}

fn verify_digest(path: &Path, expected: &str) -> Result<()> {
    let actual = digest(path)?;
    if actual != expected {
        bail!(
            "source checksum mismatch for {}: expected {expected}, found {actual}",
            path.display()
        );
    }
    Ok(())
}

fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hex::encode(hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_rejects_changed_or_missing_installed_bytes() {
        let root = tempfile::tempdir().expect("temporary install");
        fs::create_dir(root.path().join("lib")).expect("lib directory");
        let library = root.path().join("lib/libavcodec.a");
        fs::write(&library, b"target library").expect("write target library");
        let before = inventory(root.path()).expect("inventory");
        verify_digest(&library, &before[Path::new("lib/libavcodec.a")]).expect("valid digest");
        fs::write(&library, b"wrong library").expect("replace target library");
        assert!(verify_digest(&library, &before[Path::new("lib/libavcodec.a")]).is_err());
        assert_ne!(inventory(root.path()).expect("changed inventory"), before);
        fs::remove_file(&library).expect("remove target library");
        assert_ne!(inventory(root.path()).expect("missing inventory"), before);
    }
}
