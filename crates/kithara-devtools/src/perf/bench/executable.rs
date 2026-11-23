use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    io::BufReader,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use cargo_metadata::Message;

use super::{
    Log,
    command::{PREFLIGHT_STRIP, sha256},
    source::Target,
};
use crate::{Ctx, cargo};

pub(super) struct Binary {
    libraries: (&'static str, OsString),
    executable: PathBuf,
    root: PathBuf,
}

pub(super) fn build(
    root: &Path,
    target: &Target,
    directory: &Path,
    ctx: &Ctx,
    cargo_json: Option<&Path>,
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
    if cargo_json.is_some() {
        command.env(PREFLIGHT_STRIP, "none").arg("-v");
    }
    if !target.features.is_empty() {
        command.args(["--features", &target.features.join(",")]);
    }
    let output = log.output(&mut command, true)?;
    if let Some(path) = cargo_json {
        fs::write(path, &output.stdout)?;
    }
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
    let libdir = log.output(
        cargo::rustc(ctx.config.tools.program("rustc"))
            .args(["--print", "target-libdir"])
            .current_dir(root),
        true,
    )?;
    let libraries = cargo::library_path(
        &executable,
        Path::new(String::from_utf8(libdir.stdout)?.trim()),
    )?;
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
            .env(self.libraries.0, &self.libraries.1);
        command
    }

    pub(super) fn ids(
        &self,
        filter: &str,
        data: &Path,
        log: &mut Log,
    ) -> Result<(Vec<String>, Vec<u8>)> {
        let output = log.output(
            self.command(data).args(["--bench", "--list", filter]),
            false,
        )?;
        let raw = std::str::from_utf8(&output.stdout)?;
        let mut seen = BTreeSet::new();
        let mut ids = Vec::new();
        for id in raw
            .lines()
            .filter_map(|line| line.strip_suffix(": benchmark"))
        {
            ensure!(seen.insert(id), "duplicate Criterion ID {id:?}");
            ids.push(id.to_owned());
        }
        ensure!(
            !ids.is_empty(),
            "benchmark filter {filter:?} selected no Criterion IDs"
        );
        Ok((ids, output.stdout))
    }

    pub(super) fn test(&self, filter: &str, data: &Path, log: &mut Log) -> Result<Vec<u8>> {
        let output = log.output(
            self.command(data).args(["--bench", "--test", filter]),
            false,
        )?;
        Ok(output.stdout)
    }

    pub(super) fn retain(&self, destination: &Path) -> Result<String> {
        fs::copy(&self.executable, destination)?;
        Ok(sha256(&fs::read(destination)?))
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

pub(super) fn callback_ids(output: &[u8]) -> Result<Vec<String>> {
    let mut lines = std::str::from_utf8(output)?.lines();
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    while let Some(line) = lines.next() {
        let Some(id) = line.strip_prefix("Testing ") else {
            ensure!(
                !matches!(
                    line.split_ascii_whitespace().next(),
                    Some("Testing" | "Success")
                ),
                "unexpected Criterion callback record {line:?}"
            );
            continue;
        };
        ensure!(!id.trim().is_empty(), "Criterion callback needs an ID");
        ensure!(
            lines.next() == Some("Success"),
            "Criterion callback {id:?} did not report Success"
        );
        ensure!(seen.insert(id), "duplicate Criterion callback {id:?}");
        ids.push(id.to_owned());
    }
    ensure!(!ids.is_empty(), "no Criterion callbacks were reported");
    Ok(ids)
}

pub(super) fn ensure_no_criterion_results(directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink()
                && !matches!(
                    name.to_str(),
                    Some(
                        "criterion"
                            | "sample.json"
                            | "estimates.json"
                            | "benchmark.json"
                            | "raw.csv"
                    )
                ),
            "unexpected Criterion output at {}",
            path.display()
        );
        if kind.is_dir() {
            ensure_no_criterion_results(&path)?;
        }
    }
    Ok(())
}
