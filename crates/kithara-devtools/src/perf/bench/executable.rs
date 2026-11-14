use std::{
    collections::BTreeSet,
    ffi::OsString,
    io::BufReader,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use cargo_metadata::Message;

use super::{Log, source::Target};
use crate::Ctx;

pub(super) struct Binary {
    libraries: OsString,
    executable: PathBuf,
    root: PathBuf,
}

pub(super) fn build(
    root: &Path,
    target: &Target,
    directory: &Path,
    ctx: &Ctx,
    log: &mut Log,
) -> Result<Binary> {
    let mut command = Command::new(ctx.config.tools.program("cargo"));
    command
        .current_dir(root)
        .env("CARGO_TARGET_DIR", directory)
        .args([
            "bench",
            "--locked",
            "-p",
            &target.package,
            "--bench",
            &target.bench,
            "--no-run",
            "--message-format=json",
        ]);
    if !target.features.is_empty() {
        command.args(["--features", &target.features.join(",")]);
    }
    let output = log.output(&mut command, true)?;
    let mut executable = None;
    for message in Message::parse_stream(BufReader::new(output.stdout.as_slice())) {
        if let Message::CompilerArtifact(artifact) = message?
            && artifact.target.is_bench()
            && artifact.target.name == target.bench
            && let Some(path) = artifact.executable
        {
            executable = Some(PathBuf::from(path));
        }
    }
    let executable = executable.context("cargo did not report a benchmark executable")?;
    let parent = executable
        .parent()
        .context("benchmark executable has no parent")?;
    let libdir = log.output(
        Command::new(ctx.config.tools.program("rustc"))
            .args(["--print", "target-libdir"])
            .current_dir(root),
        true,
    )?;
    let variable = if cfg!(target_os = "macos") {
        "DYLD_FALLBACK_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let mut paths = vec![
        parent.to_path_buf(),
        parent
            .parent()
            .context("benchmark profile directory")?
            .to_path_buf(),
        PathBuf::from(String::from_utf8(libdir.stdout)?.trim()),
    ];
    if let Some(existing) = std::env::var_os(variable) {
        paths.extend(std::env::split_paths(&existing));
    }
    let libraries = std::env::join_paths(paths)?;
    let root = root.into();
    Ok(Binary {
        libraries,
        executable,
        root,
    })
}

impl Binary {
    fn command(&self, data: &Path) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .current_dir(&self.root)
            .env("CRITERION_HOME", data)
            .env(
                if cfg!(target_os = "macos") {
                    "DYLD_FALLBACK_LIBRARY_PATH"
                } else {
                    "LD_LIBRARY_PATH"
                },
                &self.libraries,
            );
        command
    }

    pub(super) fn ids(&self, filter: &str, data: &Path, log: &mut Log) -> Result<BTreeSet<String>> {
        let output = log.output(
            self.command(data).args(["--bench", "--list", filter]),
            false,
        )?;
        let ids: BTreeSet<_> = String::from_utf8(output.stdout)?
            .lines()
            .filter_map(|line| line.strip_suffix(": benchmark").map(str::to_owned))
            .collect();
        ensure!(
            !ids.is_empty(),
            "benchmark filter {filter:?} selected no Criterion IDs"
        );
        Ok(ids)
    }

    pub(super) fn measure(
        &self,
        filter: &str,
        name: &str,
        samples: &str,
        data: &Path,
        log: &mut Log,
    ) -> Result<()> {
        log.output(
            self.command(data).args([
                "--bench",
                filter,
                "--sample-size",
                samples,
                "--save-baseline",
                name,
            ]),
            false,
        )?;
        Ok(())
    }
}
