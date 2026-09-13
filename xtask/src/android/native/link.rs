use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::FileLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::child;

#[derive(Deserialize, Serialize)]
pub(super) struct Config {
    pub(super) compiler: PathBuf,
    pub(super) target: String,
    pub(super) source: PathBuf,
    pub(super) bridge: PathBuf,
    pub(super) objects: PathBuf,
}

pub(crate) fn run(args: &[OsString]) -> Result<()> {
    let path = env::var_os("KITHARA_ANDROID_LINK_CONFIG")
        .context("Android linker configuration is missing")?;
    let config: Config = serde_json::from_slice(&fs::read(path)?)?;
    let test = args.iter().any(|arg| archive(arg, "libtest-"));
    let mut command = Command::new(&config.compiler);
    if !test {
        command.args(args);
    } else {
        let context = context_metadata(args)?;
        let object = context
            .as_deref()
            .map(|context| bootstrap(&config, context))
            .transpose()?;
        command.args(args.iter().filter(|arg| arg.as_os_str() != "-pie"));
        command.arg("-shared").arg(&config.bridge);
        if let Some(object) = object {
            command.arg(object);
        }
    }
    let status = child::run(&mut command, None)?;
    if !status.success() {
        bail!("Android linker failed: {status}");
    }
    Ok(())
}

fn archive(arg: &std::ffi::OsStr, prefix: &str) -> bool {
    Path::new(arg).file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name.starts_with(prefix) && name.ends_with(".rlib")
    })
}

fn shared_graph(arg: &std::ffi::OsStr) -> bool {
    Path::new(arg).file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name.starts_with("libkithara_test_dylib") && name.ends_with(".so")
    })
}

fn context_metadata(args: &[OsString]) -> Result<Option<PathBuf>> {
    let graphs: Vec<_> = args.iter().filter(|arg| shared_graph(arg)).collect();
    match graphs.as_slice() {
        [graph] => {
            // rustc will not compile against the dylib as crate ndk_context; the
            // sibling rlib is that crate from the same graph. Linking it would
            // define a second ANDROID_CONTEXT inside the test image.
            context_archive(Path::new(graph)).map(Some)
        }
        [] => {
            let archives: Vec<_> = args
                .iter()
                .filter(|arg| archive(arg, "libndk_context-"))
                .collect();
            match archives.as_slice() {
                [] => Ok(None),
                [path] => Ok(Some(PathBuf::from(path))),
                _ => bail!("test link contains multiple ndk-context archives"),
            }
        }
        _ => bail!("test link contains multiple shared graph libraries"),
    }
}

fn context_archive(graph: &Path) -> Result<PathBuf> {
    let dir = graph
        .parent()
        .context("shared graph library has no parent directory")?;
    let mut archives = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("reading {} for ndk-context metadata", dir.display()))?
    {
        let path = entry?.path();
        if archive(path.as_os_str(), "libndk_context-") {
            archives.push(path);
        }
    }
    match archives.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!("ndk-context archive missing next to {}", graph.display()),
        _ => bail!("multiple ndk-context archives next to {}", graph.display()),
    }
}

fn bootstrap(config: &Config, context: &Path) -> Result<PathBuf> {
    let hash = hex::encode(Sha256::digest(fs::read(context)?));
    let object = config.objects.join(format!("{hash}.o"));
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config.objects.join(format!("{hash}.lock")))?;
    let _lock = FileLock::exclusive(lock)?;
    if !object.exists() {
        let temporary = object.with_extension(format!("{}.tmp", std::process::id()));
        let status = child::run(
            Command::new("rustc")
                .args([
                    "--edition=2024",
                    "--crate-name",
                    "android_test_bootstrap",
                    "--crate-type",
                    "lib",
                    "--emit=obj",
                    "--target",
                    &config.target,
                ])
                .arg(&config.source)
                .arg("--extern")
                .arg(format!("ndk_context={}", context.display()))
                .args(["-C", "relocation-model=pic", "-o"])
                .arg(&temporary),
            None,
        )?;
        if !status.success() {
            bail!("Android context bootstrap compilation failed");
        }
        fs::rename(temporary, &object)?;
    }
    Ok(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_libtest_links_receive_the_android_bootstrap() {
        assert!(archive(
            std::ffi::OsStr::new("/sysroot/lib/libtest-abcd.rlib"),
            "libtest-"
        ));
        assert!(!archive(
            std::ffi::OsStr::new("/deps/libtest_utils-abcd.rlib"),
            "libtest-"
        ));
        assert!(!archive(std::ffi::OsStr::new("/tmp/test.rs"), "libtest-"));
    }

    #[test]
    fn bootstrap_compiles_against_the_shared_graph_library() {
        assert!(shared_graph(std::ffi::OsStr::new(
            "/deps/libkithara_test_dylib.so"
        )));
        assert!(shared_graph(std::ffi::OsStr::new(
            "/deps/libkithara_test_dylib-0123abcd.so"
        )));
        assert!(!shared_graph(std::ffi::OsStr::new(
            "/jni/libkithara_test_abc.so"
        )));
        assert!(!shared_graph(std::ffi::OsStr::new(
            "/deps/libkithara_test_dylib.rlib"
        )));
    }

    #[test]
    fn ndk_context_metadata_is_the_archive_next_to_the_shared_graph() {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("libkithara_test_dylib.so");
        fs::write(&graph, []).unwrap();
        fs::write(dir.path().join("libndk_context-abcd.rlib"), []).unwrap();
        assert_eq!(
            context_archive(&graph).unwrap(),
            dir.path().join("libndk_context-abcd.rlib")
        );
        assert_eq!(
            context_metadata(&[graph.into()]).unwrap().as_deref(),
            Some(dir.path().join("libndk_context-abcd.rlib").as_path())
        );
    }

    #[test]
    fn test_images_without_ndk_context_skip_the_rust_bootstrap() {
        assert_eq!(
            context_metadata(&[OsString::from("/sysroot/lib/libtest-abcd.rlib")]).unwrap(),
            None
        );
        assert_eq!(
            context_metadata(&[OsString::from("/deps/libndk_context-abcd.rlib")])
                .unwrap()
                .as_deref(),
            Some(Path::new("/deps/libndk_context-abcd.rlib"))
        );
    }
}
