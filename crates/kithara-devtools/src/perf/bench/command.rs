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
    case::Case,
    dispatch,
    executable::{Binary, build, callback_ids, ensure_no_criterion_results},
    result::{AllocatorPolicy, BenchMode, read_allocation, read_observations},
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
    #[arg(long = "bench-mode", env = "BENCH_MODE", default_value_t = BenchMode::Timing)]
    pub(super) mode: BenchMode,
    #[arg(long = "bench-allocator-policy", env = "BENCH_ALLOCATOR_POLICY", default_value_t = AllocatorPolicy::System)]
    pub(super) allocator_policy: AllocatorPolicy,
}

impl Request {
    pub(super) fn parameters(&self) -> [(&'static str, &str); 6] {
        [
            ("BENCH_TARGET", &self.target),
            ("BENCH_FILTER", &self.filter),
            ("BENCH_REF", &self.head),
            ("BENCH_BASE_REF", &self.base),
            ("BENCH_MODE", self.mode.as_str()),
            ("BENCH_ALLOCATOR_POLICY", self.allocator_policy.as_str()),
        ]
    }
}

const PREFLIGHT_ENV: &str = "BENCH_PREFLIGHT";
pub(super) const PREFLIGHT_STRIP: &str = "CARGO_PROFILE_BENCH_STRIP";
const PREFLIGHT_BINARY_DIR: &str = if cfg!(target_os = "macos") {
    "mach-o"
} else {
    "elf"
};
const PREFLIGHT_BINARY_FORMAT: &str = if cfg!(target_os = "macos") {
    "Mach-O"
} else {
    "ELF"
};

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
    cases: Vec<Case>,
    base_cases: Option<Vec<Case>>,
}

impl Preflight {
    pub(super) fn parse(request: &Request, input: &str, measure: bool) -> Result<Self> {
        ensure!(
            !measure,
            "preflight cannot run with benchmark timing enabled"
        );
        ensure!(
            std::env::var_os("CRITERION_DEBUG").is_none(),
            "benchmark preflight requires CRITERION_DEBUG to be unset"
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
                    && !["elf", "mach-o", "provenance.txt"]
                        .iter()
                        .any(|reserved| name.eq_ignore_ascii_case(reserved)),
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
            validate_cases(name, &collection.cases, &collection.expected_head_ids)?;
            match (&collection.expected_base_ids, &collection.base_cases) {
                (Some(base_ids), Some(base_cases)) => {
                    validate_cases(name, base_cases, base_ids)?;
                    ensure!(
                        base_cases == &collection.cases,
                        "preflight collection {name:?} declares different base/head parameters or boundaries"
                    );
                    ensure!(
                        base_ids.iter().collect::<BTreeSet<_>>()
                            == collection.expected_head_ids.iter().collect(),
                        "preflight collection {name:?} declares different base/head IDs"
                    );
                }
                (Some(_), None) => anyhow::bail!(
                    "preflight collection {name:?} has base IDs without base case declarations"
                ),
                (None, Some(_)) => anyhow::bail!(
                    "preflight collection {name:?} has base case declarations without base IDs"
                ),
                (None, None) => {}
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
        results: &Path,
        log: &mut Log,
    ) -> Result<()> {
        let mut checks = Vec::new();
        let mut collections: Vec<_> = self.collections.iter().collect();
        collections.sort_by_key(|(_, collection)| collection.expected_base_ids.is_none());
        for (name, collection) in collections {
            let arms = collection
                .expected_base_ids
                .iter()
                .zip(collection.base_cases.iter())
                .map(|(ids, cases)| ("base", ids, cases, binaries[0]))
                .chain(std::iter::once((
                    "head",
                    &collection.expected_head_ids,
                    &collection.cases,
                    binaries[1],
                )));
            for (arm, expected, cases, binary) in arms {
                let directory = results.join(name).join(arm);
                fs::create_dir(&directory)?;
                let (listed, output) = binary.ids(&collection.filter, &directory, log)?;
                fs::write(directory.join("list.txt"), output)?;
                let admitted: Vec<_> = cases.iter().map(Case::admitted).collect();
                fs::write(
                    directory.join("cases.json"),
                    serde_json::to_vec_pretty(&admitted)?,
                )?;
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
            check_ids(expected, &listed.iter().cloned().collect())
                .with_context(|| format!("preflight inventory {}", directory.display()))?;
        }
        for (filter, directory, binary, _, listed) in checks {
            let output = binary.test(filter, &directory, log)?;
            fs::write(directory.join("test.txt"), &output)?;
            let callbacks = callback_ids(&output)
                .with_context(|| format!("preflight callbacks {}", directory.display()))?;
            ensure!(
                listed == callbacks,
                "preflight callbacks {} differ from inventory order; listed {listed:?}, ran {callbacks:?}",
                directory.display()
            );
            ensure_no_criterion_results(&directory)?;
        }

        let binary_dir = results.join(PREFLIGHT_BINARY_DIR);
        let mut provenance = build_provenance(ctx, sources, digest, log)?;
        for (arm, binary) in ["base", "head"].into_iter().zip(binaries) {
            let hash = binary.retain(&binary_dir.join(arm))?;
            writeln!(provenance, "{arm} {PREFLIGHT_BINARY_FORMAT} SHA-256 {hash}")?;
        }
        log.line(provenance.trim_end())?;
        fs::write(results.join("provenance.txt"), provenance)?;
        log.line(&format!(
            "Untimed benchmark preflight evidence: {}",
            results.display()
        ))
    }
}

fn validate_cases(name: &str, cases: &[Case], expected_ids: &[String]) -> Result<()> {
    ensure!(
        !cases.is_empty(),
        "preflight collection {name:?} has no case declarations"
    );
    for case in cases {
        case.validate()
            .with_context(|| format!("preflight collection {name:?}"))?;
    }
    let declared: BTreeSet<_> = cases.iter().map(|case| case.id.as_str()).collect();
    ensure!(
        declared.len() == cases.len(),
        "preflight collection {name:?} has duplicate case declarations"
    );
    let expected: BTreeSet<_> = expected_ids.iter().map(String::as_str).collect();
    ensure!(
        declared == expected,
        "preflight collection {name:?} case declarations do not match expected IDs"
    );
    Ok(())
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
        preflight.is_none() || cfg!(any(target_os = "linux", target_os = "macos")),
        "benchmark preflight requires Linux or macOS"
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
    let preflight = preflight
        .map(|(declaration, digest)| -> Result<_> {
            ensure!(
                head.len() == 1 && base.is_some(),
                "preflight requires one target built for both arms"
            );
            let parent = ctx.root.join("bench-results");
            fs::create_dir_all(&parent)?;
            let results = Builder::new()
                .prefix("preflight-")
                .tempdir_in(parent)?
                .keep();
            fs::create_dir(results.join(PREFLIGHT_BINARY_DIR))?;
            for name in declaration.collections.keys() {
                let directory = results.join(name);
                fs::create_dir(&directory).with_context(|| {
                    format!("create preflight collection {}", directory.display())
                })?;
            }
            Ok((declaration, digest, results))
        })
        .transpose()?;
    let cargo_json = |arm: &str| {
        preflight.as_ref().map(|(_, _, results)| {
            results
                .join(PREFLIGHT_BINARY_DIR)
                .join(format!("{arm}.cargo.json"))
        })
    };
    let mut binaries = Vec::new();
    for (index, target) in head.iter().enumerate() {
        let base = if let Some(root) = &sources.base {
            Some(build(
                root,
                target,
                &sources.scratch.path().join(format!("base-build-{index}")),
                ctx,
                cargo_json("base").as_deref(),
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
            cargo_json("head").as_deref(),
            &mut log,
        )?;
        binaries.push((base, head));
    }
    if let Some((declaration, digest, results)) = preflight {
        let [(Some(base), head)] = binaries.as_slice() else {
            anyhow::bail!("preflight requires one target built for both arms");
        };
        return declaration.collect(ctx, &sources, [base, head], &digest, &results, &mut log);
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
    let mut selected = Vec::new();
    for ((base, head), target) in binaries.into_iter().zip(&head) {
        let (ids, _) = head.ids(&request.filter, &data, &mut log)?;
        let ids: BTreeSet<_> = ids.into_iter().collect();
        if let Some(base) = &base {
            let (base_ids, _) = base.ids(&request.filter, &data, &mut log)?;
            let base_ids: BTreeSet<_> = base_ids.into_iter().collect();
            ensure!(
                base_ids == ids,
                "base/head Criterion IDs do not match; {}",
                selection_difference(&base_ids, &ids)
            );
        }
        selected.push((base, head, target, ids));
    }
    if request.mode == BenchMode::Allocation {
        return run_allocation(selected, request, &results, &data, &mut log);
    }
    for (base, head, _, _) in selected {
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

type Selected<'a> = (Option<Binary>, Binary, &'a Target, BTreeSet<String>);

fn run_allocation(
    selected: Vec<Selected<'_>>,
    request: &Request,
    results: &Path,
    data: &Path,
    log: &mut Log,
) -> Result<()> {
    for (base, head, target, ids) in selected {
        for (arm, binary) in [("base", base.as_ref()), ("head", Some(&head))]
            .into_iter()
            .filter_map(|(arm, binary)| binary.map(|binary| (arm, binary)))
        {
            let raw = results.join(format!("{}-{arm}-allocation.json", target.bench));
            let markdown = results.join(format!("{}-{arm}-allocation.md", target.bench));
            let observations = results.join(format!("{}-{arm}-observations.jsonl", target.bench));
            binary.allocate(&request.filter, &raw, &markdown, &observations, data, log)?;
            let report = read_allocation(&raw)?;
            let owner_observations = read_observations(&observations, request.allocator_policy)?;
            let actual = report.ids();
            ensure!(
                actual == ids,
                "allocation IDs do not match Criterion IDs; {}",
                selection_difference(&ids, &actual)
            );
            let target_id = target.id();
            let canonical = report.canonical(
                &target_id,
                &target.features,
                request.allocator_policy,
                &owner_observations,
            )?;
            let path = results.join(format!("{}-{arm}-canonical.json", target.bench));
            canonical.write(&path)?;
            log.line(&format!("Canonical allocation result: {}", path.display()))?;
        }
    }
    log.line(&format!("Allocation results: {}", results.display()))
}

fn selection_difference(base: &BTreeSet<String>, head: &BTreeSet<String>) -> String {
    format!(
        "only in base: {:?}; only in head: {:?}",
        base.difference(head).collect::<Vec<_>>(),
        head.difference(base).collect::<Vec<_>>()
    )
}
