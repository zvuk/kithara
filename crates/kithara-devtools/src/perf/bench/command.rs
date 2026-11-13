use std::{
    fs::{self, File},
    io::{self, Write},
    process::{Command, Output},
};

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use serde::Deserialize;
use tempfile::Builder;

use super::{
    dispatch,
    executable::build,
    source::{Sources, metadata, select, targets},
};
use crate::{Ctx, verdict::ChildFailure};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[derive(Debug, Args)]
pub(crate) struct BenchArgs {
    #[command(subcommand)]
    command: BenchCommand,
}

#[derive(Debug, Subcommand)]
enum BenchCommand {
    /// Print workspace bench targets and manifest-required features.
    List,
    /// Compile the selected benchmarks without running them.
    Build(Request),
    /// Execute selected benchmarks.
    Run(Request),
    /// Compile, then execute only when `RUN_BENCHMARKS=1`.
    Ci(Request),
    /// Start the existing Linux or Mac CI benchmark job.
    Dispatch(dispatch::DispatchArgs),
}

#[derive(Clone, Debug, Default, Args)]
pub(super) struct Request {
    #[arg(long = "bench-base-ref", env = "BENCH_BASE_REF", default_value = "")]
    pub(super) base: String,
    #[arg(long = "bench-filter", env = "BENCH_FILTER", default_value = "")]
    pub(super) filter: String,
    #[arg(long = "bench-ref", env = "BENCH_REF", default_value = "")]
    pub(super) head: String,
    #[arg(long = "bench-target", env = "BENCH_TARGET", default_value = "")]
    pub(super) target: String,
}

impl Request {
    pub(super) fn parameters(&self) -> [(&'static str, &str); 4] {
        [
            ("BENCH_TARGET", &self.target),
            ("BENCH_FILTER", &self.filter),
            ("BENCH_REF", &self.head),
            ("BENCH_BASE_REF", &self.base),
        ]
    }
}

#[derive(Deserialize)]
pub(super) struct Deployment {
    pub(super) gitlab_project: String,
    pub(super) repository: String,
}

impl Deployment {
    pub(super) fn load(ctx: &Ctx) -> Result<Self> {
        ctx.config
            .ext
            .get("perf")
            .and_then(|perf| perf.get("bench"))
            .context("configure ext.perf.bench for benchmark dispatch/source refs")?
            .clone()
            .try_into()
            .context("parse benchmark deployment")
    }
}

pub(crate) fn run(args: &BenchArgs, ctx: &Ctx) -> Result<()> {
    match &args.command {
        BenchCommand::List => {
            for target in targets(ctx.metadata()?) {
                println!("{}", target.id());
            }
            Ok(())
        }
        BenchCommand::Dispatch(args) => dispatch::run(args, ctx),
        BenchCommand::Build(request) => execute(ctx, request, false),
        BenchCommand::Run(request) => execute(ctx, request, true),
        BenchCommand::Ci(request) => execute(
            ctx,
            request,
            std::env::var("RUN_BENCHMARKS").as_deref() == Ok("1"),
        ),
    }
}

pub(super) struct Log(File);

impl Log {
    pub(super) fn line(&mut self, text: &str) -> Result<()> {
        println!("{text}");
        writeln!(self.0, "{text}")?;
        Ok(())
    }

    pub(super) fn output(&mut self, command: &mut Command, json: bool) -> Result<Output> {
        self.line(&format!("$ {command:?}"))?;
        let output = command.output().context("start benchmark command")?;
        if !json || !output.status.success() {
            io::stdout().write_all(&output.stdout)?;
            self.0.write_all(&output.stdout)?;
        }
        io::stderr().write_all(&output.stderr)?;
        self.0.write_all(&output.stderr)?;
        if !output.status.success() {
            return Err(ChildFailure::inherited(
                "benchmark command".into(),
                output.status.code(),
            ));
        }
        Ok(output)
    }
}

fn execute(ctx: &Ctx, request: &Request, measure: bool) -> Result<()> {
    ensure!(
        request.target.is_empty() || !request.target.chars().any(char::is_control),
        "invalid benchmark target"
    );
    ensure!(
        (request.base.is_empty() && request.filter.is_empty()) || !request.target.is_empty(),
        "a filter/comparison requires one --bench-target"
    );
    ensure!(
        !request.filter.starts_with('-'),
        "benchmark filter must not be a command option"
    );
    let mut log = Log(File::create(ctx.root.join("bench-results.txt"))?);
    let sources = Sources::prepare(ctx, request, &mut log)?;
    let cargo = ctx.config.tools.program("cargo");
    let head = select(&metadata(&sources.head, cargo)?, &request.target)?;
    ensure!(!head.is_empty(), "workspace contains no benchmark targets");
    let base = sources
        .base
        .as_ref()
        .map(|root| metadata(root, cargo).and_then(|metadata| select(&metadata, &request.target)))
        .transpose()?;
    if let Some(base) = &base {
        ensure!(
            base == &head,
            "base/head benchmark targets or required features differ"
        );
    }
    let mut binaries = Vec::new();
    for (index, target) in head.iter().enumerate() {
        let base = if let Some(root) = &sources.base {
            Some(build(
                root,
                target,
                Some(&sources.scratch.path().join(format!("base-build-{index}"))),
                ctx,
                &mut log,
            )?)
        } else {
            None
        };
        let head_directory = sources
            .base
            .as_ref()
            .map(|_| sources.scratch.path().join(format!("head-build-{index}")));
        let head = build(
            &sources.head,
            target,
            head_directory.as_deref(),
            ctx,
            &mut log,
        )?;
        binaries.push((base, head));
    }
    if !measure {
        return log
            .line("==> benchmark execution skipped (set RUN_BENCHMARKS=1 in ci mode to execute)");
    }
    let parent = ctx.root.join(".ci-artifacts/bench");
    fs::create_dir_all(&parent)?;
    let results = Builder::new().prefix("bench-").tempdir_in(parent)?.keep();
    let data = results.join("criterion");
    fs::create_dir_all(&data)?;
    let samples = std::env::var("BENCH_SAMPLE_SIZE").unwrap_or_else(|_| "20".into());
    ensure!(
        samples.parse::<usize>().is_ok_and(|samples| samples >= 10),
        "BENCH_SAMPLE_SIZE must be an integer >=10"
    );
    for (base, head) in &binaries {
        let ids = head.ids(&request.filter, &data, &mut log)?;
        if let Some(base) = base {
            ensure!(
                base.ids(&request.filter, &data, &mut log)? == ids,
                "base/head Criterion IDs do not match"
            );
        }
    }
    for (base, head) in binaries {
        if let Some(base) = &base {
            base.measure(&request.filter, "base", &samples, &data, &mut log)?;
        }
        head.measure(&request.filter, "head", &samples, &data, &mut log)?;
    }
    if sources.base.is_some() {
        log.output(
            Command::new(ctx.config.tools.program("critcmp"))
                .arg("--target-dir")
                .arg(&results)
                .args(["base", "head"])
                .current_dir(&ctx.root),
            false,
        )?;
    }
    log.line(&format!("Criterion results: {}", results.display()))
}
