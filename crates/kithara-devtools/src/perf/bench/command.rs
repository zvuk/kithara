use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path},
    process::{Command, Output, Stdio},
    thread,
};

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::Builder;

use super::{
    dispatch,
    executable::{Binary, build, callback_ids, ensure_no_criterion_results},
    source::{Sources, Target, build_provenance, metadata, select, target_parts, targets},
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

const PREFLIGHT_ENV: &str = "BENCH_PREFLIGHT";
pub(super) const PREFLIGHT_STRIP: &str = "CARGO_PROFILE_BENCH_STRIP";

#[derive(Deserialize)]
pub(super) struct Preflight {
    head: String,
    collections: BTreeMap<String, Collection>,
}

#[derive(Deserialize)]
struct Collection {
    filter: String,
    expected_head_ids: Vec<String>,
    expected_base_ids: Option<Vec<String>>,
}

impl Preflight {
    pub(super) fn parse(request: &Request, input: &str, measure: bool) -> Result<Self> {
        ensure!(
            !measure,
            "preflight cannot run with benchmark timing enabled"
        );
        target_parts(&request.target)?;
        ensure!(
            request.filter.is_empty(),
            "preflight filters belong to the declaration; leave BENCH_FILTER empty"
        );
        ensure!(
            [&request.base, &request.head].into_iter().all(|sha| {
                sha.len() == 40
                    && sha
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            }),
            "preflight requires full lowercase base and head commit SHAs"
        );
        let declaration: Self =
            serde_json::from_str(input).context("parse preflight declaration")?;
        ensure!(
            declaration.head == request.head,
            "preflight declaration head does not match BENCH_REF"
        );
        ensure!(
            !declaration.collections.is_empty(),
            "preflight has no collections"
        );
        for (name, collection) in &declaration.collections {
            let mut components = Path::new(name).components();
            ensure!(
                components.next() == Some(Component::Normal(name.as_ref()))
                    && components.next().is_none()
                    && !name.contains(['/', '\\', ':'])
                    && !name.chars().any(char::is_control)
                    && !matches!(name.as_str(), "elf" | "provenance.txt"),
                "preflight collection name must be one safe path component"
            );
            ensure!(
                !collection.filter.trim().is_empty()
                    && !collection.filter.starts_with('-')
                    && !collection.filter.chars().any(char::is_control),
                "preflight collection {name:?} needs a valid filter"
            );
            for ids in std::iter::once(&collection.expected_head_ids)
                .chain(collection.expected_base_ids.iter())
            {
                ensure!(
                    !ids.is_empty()
                        && ids.iter().all(|id| !id.trim().is_empty())
                        && ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
                    "preflight collection {name:?} has empty or duplicate expected IDs"
                );
            }
            if let Some(base_ids) = &collection.expected_base_ids {
                ensure!(
                    base_ids.iter().collect::<BTreeSet<_>>()
                        == collection.expected_head_ids.iter().collect(),
                    "preflight collection {name:?} declares different base/head IDs"
                );
            }
        }
        Ok(declaration)
    }

    fn collect(
        self,
        ctx: &Ctx,
        sources: &Sources,
        binaries: [&Binary; 2],
        digest: &str,
        log: &mut Log,
    ) -> Result<()> {
        let parent = ctx.root.join("bench-results");
        fs::create_dir_all(&parent)?;
        let results = Builder::new()
            .prefix("preflight-")
            .tempdir_in(parent)?
            .keep();
        let mut checks = Vec::new();
        let mut collections: Vec<_> = self.collections.iter().collect();
        collections.sort_by_key(|(_, collection)| collection.expected_base_ids.is_none());
        for (name, collection) in collections {
            let arms = collection
                .expected_base_ids
                .iter()
                .map(|ids| ("base", ids, binaries[0]))
                .chain(std::iter::once((
                    "head",
                    &collection.expected_head_ids,
                    binaries[1],
                )));
            for (arm, expected, binary) in arms {
                let directory = results.join(name).join(arm);
                fs::create_dir_all(&directory)?;
                let (listed, output) = binary.ids(&collection.filter, &directory, log)?;
                fs::write(directory.join("list.txt"), output)?;
                let expected = expected.iter().cloned().collect();
                checks.push((
                    collection.filter.as_str(),
                    directory,
                    binary,
                    expected,
                    listed,
                ));
            }
        }
        for (_, directory, _, expected, listed) in &checks {
            check_ids(expected, listed)
                .with_context(|| format!("preflight inventory {}", directory.display()))?;
        }
        for (filter, directory, binary, _, listed) in checks {
            let output = binary.test(filter, &directory, log)?;
            fs::write(directory.join("test.txt"), &output)?;
            check_ids(&listed, &callback_ids(&output)?)
                .with_context(|| format!("preflight callbacks {}", directory.display()))?;
            ensure_no_criterion_results(&directory)?;
        }

        let elf = results.join("elf");
        fs::create_dir(&elf)?;
        let mut provenance = build_provenance(ctx, sources, digest, log)?;
        for (arm, binary) in ["base", "head"].into_iter().zip(binaries) {
            let hash = binary.retain(&elf.join(arm))?;
            writeln!(provenance, "{arm} ELF SHA-256 {hash}")?;
        }
        log.line(provenance.trim_end())?;
        fs::write(results.join("provenance.txt"), provenance)?;
        log.line(&format!(
            "Untimed benchmark preflight evidence: {}",
            results.display()
        ))
    }
}

fn check_ids(expected: &BTreeSet<String>, observed: &BTreeSet<String>) -> Result<()> {
    ensure!(
        expected == observed,
        "Criterion IDs do not match; {}",
        selection_difference(expected, observed)
    );
    Ok(())
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Deserialize)]
pub(super) struct Deployment {
    pub(super) github_repo: String,
    pub(super) gitlab_host: String,
    pub(super) gitlab_project: String,
}

impl Deployment {
    pub(super) fn load(ctx: &Ctx) -> Result<Self> {
        ctx.config
            .ext
            .get("release")
            .context("configure existing ext.release repository settings for benchmark dispatch")?
            .clone()
            .try_into()
            .context("parse benchmark deployment")
    }
}

pub(crate) fn run(args: &BenchArgs, ctx: &Ctx) -> Result<()> {
    let (request, measure) = match &args.command {
        BenchCommand::List => {
            for target in targets(ctx.metadata()?) {
                println!("{}", target.id());
            }
            return Ok(());
        }
        BenchCommand::Dispatch(args) => return dispatch::run(args, ctx),
        BenchCommand::Build(request) => (request, false),
        BenchCommand::Run(request) => (request, true),
        BenchCommand::Ci(request) => (
            request,
            std::env::var("RUN_BENCHMARKS").as_deref() == Ok("1"),
        ),
    };
    let preflight = std::env::var_os(PREFLIGHT_ENV).filter(|value| !value.is_empty());
    let preflight = preflight
        .as_ref()
        .map(|value| value.to_str().context("BENCH_PREFLIGHT must be UTF-8"))
        .transpose()?;
    ensure!(
        preflight.is_none() || cfg!(target_os = "linux"),
        "benchmark preflight requires Linux"
    );
    execute(ctx, request, measure, preflight)
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
        let output = stream_output(command, &mut self.0).context("start benchmark command")?;
        if !json || !output.status.success() {
            io::stdout().write_all(&output.stdout)?;
            self.0.write_all(&output.stdout)?;
        }
        if !output.status.success() {
            return Err(ChildFailure::inherited(
                "benchmark command".into(),
                output.status.code(),
            ));
        }
        Ok(output)
    }
}

/// Runs `command` while teeing its stderr to this process's stderr and the
/// benchmark log as it is read, so a long build is observable before it exits
/// and a killed build still leaves its diagnostics in the log. Stdout is
/// drained concurrently on its own thread and stays captured untouched, so a
/// child that fills the stdout pipe cannot deadlock the parent and
/// `--message-format=json` remains parseable.
///
/// Wait for the child and join the stdout reader before returning an I/O error.
/// Preserve the first error when cleanup also fails.
fn stream_output(command: &mut Command, log: &mut File) -> io::Result<Output> {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("benchmark stdout is piped");
    let mut child_stderr = child.stderr.take().expect("benchmark stderr is piped");
    // Drain stdout on its own thread so the child is never blocked on a full
    // stdout pipe while we read stderr.
    let stdout_thread = thread::spawn(move || {
        let mut collected = Vec::new();
        stdout.read_to_end(&mut collected).map(|_| collected)
    });

    // Tee stderr, preserving the first I/O error instead of returning early,
    // so the child is still reaped and the reader joined on every exit.
    let mut stderr_collected = Vec::new();
    let mut primary_error = None;
    let mut buffer = [0_u8; 4096];
    loop {
        let read = match child_stderr.read(&mut buffer) {
            Ok(read) => read,
            Err(error) => {
                primary_error = primary_error.or(Some(error));
                break;
            }
        };
        if read == 0 {
            break;
        }
        let bytes = &buffer[..read];
        if let Err(error) = io::stderr().write_all(bytes) {
            primary_error = primary_error.or(Some(error));
            break;
        }
        if let Err(error) = log.write_all(bytes) {
            primary_error = primary_error.or(Some(error));
            break;
        }
        stderr_collected.extend_from_slice(bytes);
    }
    drop(child_stderr);
    let (stdout_collected, join_error) = match stdout_thread.join() {
        Ok(Ok(stdout)) => (stdout, None),
        Ok(Err(error)) => (Vec::new(), Some(error)),
        Err(_) => (
            Vec::new(),
            Some(io::Error::other("benchmark stdout reader panicked")),
        ),
    };
    let (status, wait_error) = match child.wait() {
        Ok(status) => (Some(status), None),
        Err(error) => (None, Some(error)),
    };
    if let Some(error) = primary_error.or(join_error).or(wait_error) {
        return Err(error);
    }
    Ok(Output {
        status: status.expect("status present when no error"),
        stdout: stdout_collected,
        stderr: stderr_collected,
    })
}

fn execute(ctx: &Ctx, request: &Request, measure: bool, input: Option<&str>) -> Result<()> {
    let preflight = input
        .map(|raw| {
            Preflight::parse(request, raw, measure).map(|parsed| (parsed, sha256(raw.as_bytes())))
        })
        .transpose()?;
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
    if let Some((_, digest)) = &preflight {
        log.line(&format!("preflight declaration SHA-256 {digest}"))?;
    }
    let sources = Sources::prepare(ctx, request, preflight.is_some(), &mut log)?;
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
            "base/head benchmark targets or required features differ; {}",
            selection_difference(
                &base.iter().map(Target::id).collect(),
                &head.iter().map(Target::id).collect()
            )
        );
    }
    let mut binaries = Vec::new();
    for (index, target) in head.iter().enumerate() {
        let base = if let Some(root) = &sources.base {
            Some(build(
                root,
                target,
                &sources.scratch.path().join(format!("base-build-{index}")),
                ctx,
                preflight.is_some(),
                &mut log,
            )?)
        } else {
            None
        };
        let head = build(
            &sources.head,
            target,
            &sources.scratch.path().join(format!("head-build-{index}")),
            ctx,
            preflight.is_some(),
            &mut log,
        )?;
        binaries.push((base, head));
    }
    if let Some((declaration, digest)) = preflight {
        let [(Some(base), head)] = binaries.as_slice() else {
            anyhow::bail!("preflight requires one target built for both arms");
        };
        return declaration.collect(ctx, &sources, [base, head], &digest, &mut log);
    }
    if !measure {
        return log
            .line("==> benchmark execution skipped (set RUN_BENCHMARKS=1 in ci mode to execute)");
    }
    let parent = ctx.root.join("bench-results");
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
        let (ids, _) = head.ids(&request.filter, &data, &mut log)?;
        if let Some(base) = base {
            let (base_ids, _) = base.ids(&request.filter, &data, &mut log)?;
            ensure!(
                base_ids == ids,
                "base/head Criterion IDs do not match; {}",
                selection_difference(&base_ids, &ids)
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

fn selection_difference(base: &BTreeSet<String>, head: &BTreeSet<String>) -> String {
    format!(
        "only in base: {:?}; only in head: {:?}",
        base.difference(head).collect::<Vec<_>>(),
        head.difference(base).collect::<Vec<_>>()
    )
}
