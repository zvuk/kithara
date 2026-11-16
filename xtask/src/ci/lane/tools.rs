use std::{
    env,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use cargo_metadata::semver::Version;
use kithara_devtools::lock::{FileLock, Wait};

use crate::ci::{config::CiPins, environment::CacheTrust, process::Process};

#[derive(Clone, Copy, Debug)]
pub(super) enum Tool {
    Nextest,
    Critcmp,
}

impl Tool {
    const fn cache_label(self) -> &'static str {
        match self {
            Self::Nextest => "nextest tool",
            Self::Critcmp => "critcmp tool",
        }
    }
    const fn name(self) -> &'static str {
        match self {
            Self::Nextest => "cargo-nextest",
            Self::Critcmp => "critcmp",
        }
    }
    const fn version_args(self) -> &'static [&'static str] {
        match self {
            Self::Nextest => &["nextest", "--version"],
            Self::Critcmp => &["--version"],
        }
    }
}

pub(super) fn prepare(
    process: &Process,
    pins: &CiPins,
    cargo: &str,
    tool: Tool,
) -> Result<Option<PathBuf>> {
    if process.is_recording() {
        return Ok(None);
    }
    let started = Instant::now();
    let name = tool.name();
    let label = name.trim_start_matches("cargo-");
    let version = pins.cargo_tool_version(name)?;
    Version::parse(version)
        .with_context(|| format!("{label} pin must be an exact semantic version"))?;
    let cache = process
        .cargo_home()
        .with_context(|| format!("CI {label} needs the executor's Cargo home"))?
        .join(format!("kithara-{label}"))
        .join(CacheTrust::read(&|name| process.environment_var(name))?.as_str())
        .join(format!("{}-{}", env::consts::OS, env::consts::ARCH));
    let root = cache.join(version);
    let bin = root.join("bin");
    let executable = bin.join(format!("{name}{}", env::consts::EXE_SUFFIX));
    fs::create_dir_all(&cache).with_context(|| format!("create the pinned {label} cache"))?;
    let installed = {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(cache.join(format!("{version}.lock")))?;
        let holder = format!("{label} {version}, pid {}", std::process::id());
        let _lock = FileLock::exclusive(
            file,
            &Wait {
                subject: &format!("pinned {label} installation"),
                holder: &holder,
            },
        )?;
        let installed = !present(&executable)?;
        if installed {
            let temp = process
                .environment_path("TMPDIR")
                .unwrap_or_else(env::temp_dir);
            let build = tempfile::tempdir_in(temp)
                .with_context(|| format!("create temporary {label} build directory"))?;
            process.run_command(
                process
                    .command(cargo)
                    .args(["install", "--locked", "--version", version, "--root"])
                    .arg(&root)
                    .arg(name)
                    .current_dir(build.path())
                    .env("CARGO_TARGET_DIR", build.path())
                    .env_remove("RUSTFLAGS")
                    .env_remove("CARGO_ENCODED_RUSTFLAGS")
                    .env_remove("RUSTDOCFLAGS")
                    .env_remove("CARGO_BUILD_TARGET"),
                &format!("install pinned {label}"),
            )?;
        }
        let actual = process.transcript(
            process.command(&executable).args(tool.version_args()),
            &format!("read cached {label} version"),
        )?;
        require_version(&actual, name, version)?;
        installed
    };
    process.note_cache(
        tool.cache_label(),
        if installed { "installed" } else { "reused" },
        format!(
            "{version} at {} in {:.1} s",
            bin.display(),
            started.elapsed().as_secs_f64()
        ),
    );
    Ok(Some(bin))
}

fn present(executable: &Path) -> Result<bool> {
    match fs::symlink_metadata(executable) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => bail!(
            "job-tool cache artifact {} is not a regular file",
            executable.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("inspect job-tool cache {}", executable.display()))
        }
    }
}

fn require_version(actual: &str, name: &str, version: &str) -> Result<()> {
    let mut words = actual.split_whitespace();
    if words.next() != Some(name) || words.next() != Some(version) {
        bail!(
            "{} version mismatch: expected {version}, got {actual}",
            name.trim_start_matches("cargo-")
        );
    }
    Ok(())
}

pub(super) fn prepend(command: &mut Command, bin: &Path) -> Result<()> {
    let path = command
        .get_envs()
        .find_map(|(name, value)| (name == "PATH").then(|| value.map(OsStr::to_os_string)))
        .unwrap_or_else(|| env::var_os("PATH"));
    let mut paths = vec![bin.to_path_buf()];
    if let Some(path) = path {
        paths.extend(env::split_paths(&path));
    }
    command.env(
        "PATH",
        env::join_paths(paths).context("scope the pinned job-tool PATH")?,
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        ci::config::fixture,
        testing::{install_copy, install_double},
    };

    #[test]
    fn bench_critcmp_reuses_the_versioned_job_tool_without_cargo_install() {
        let temp = tempfile::tempdir().unwrap();
        let cargo_home = temp.path().join("cargo");
        let pins = fixture().pins;
        let version = pins.cargo_tool_version("critcmp").unwrap();
        let bin = cargo_home
            .join("kithara-critcmp")
            .join("review")
            .join(format!("{}-{}", env::consts::OS, env::consts::ARCH))
            .join(version)
            .join("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = install_double(&temp.path().join("double"), "critcmp");
        let installed = bin.join(format!("critcmp{}", env::consts::EXE_SUFFIX));
        install_copy(&fake, &installed);
        let trace = temp.path().join("trace");
        let process = Process::new(
            temp.path(),
            BTreeMap::from([
                ("CARGO_HOME".into(), cargo_home.into_os_string()),
                ("KITHARA_CACHE_TRUST".into(), "review".into()),
                (
                    "KITHARA_TEST_STDOUT".into(),
                    format!("critcmp {version}\n").into(),
                ),
                ("KITHARA_TEST_TRACE".into(), trace.clone().into_os_string()),
            ]),
        );
        for _ in 0..2 {
            assert_eq!(
                prepare(&process, &pins, "missing-cargo", Tool::Critcmp).unwrap(),
                Some(bin.clone())
            );
        }
        assert_eq!(fs::read_to_string(trace).unwrap(), "--version\n--version\n");
        assert!(installed.is_file());
        assert!(require_version("critcmp 99", "critcmp", version).is_err());
    }
}
