use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::{DependencyKind, MetadataCommand};
use kithara_devtools::{Ctx, util::check_tool};

use crate::config::{KitharaExt, PublishConfig};

struct Consts;

impl Consts {
    /// User-agent used for registry availability checks when the project
    /// config leaves `publish.user_agent` empty.
    const DEFAULT_USER_AGENT: &'static str = "xtask-publish";
}

#[derive(Debug, clap::Args)]
pub(crate) struct PublishArgs {
    /// Perform a dry run (pass --dry-run to cargo publish).
    #[arg(long)]
    dry_run: bool,

    /// During dry-run, verify publishable crates whose workspace deps are
    /// already available on crates.io using `cargo publish --dry-run`.
    #[arg(long, requires = "dry_run")]
    verify_registry: bool,

    /// Delay in seconds between publishes. Skipped during dry-run. New crates
    /// additionally keep to the registry's pace from `[ext.publish]`.
    #[arg(long)]
    delay: Option<u64>,

    /// Skip the verification build (`cargo publish --no-verify`). Required for
    /// workspace library crates that leave the HTTP backend to the consumer:
    /// an isolated default-feature build selects no backend and fails to
    /// compile even though the packaged source is correct.
    #[arg(long)]
    no_verify: bool,
}

pub(crate) fn run(args: &PublishArgs, ctx: &Ctx) -> Result<()> {
    check_tool(
        ctx.config.tools.program("cargo-hakari"),
        &["hakari", "--version"],
        ctx.config
            .tools
            .install_hint("cargo-hakari", "cargo install cargo-hakari"),
    )?;

    let order = resolve_publish_order()?;

    if order.is_empty() {
        println!("No publishable crates found.");
        return Ok(());
    }

    println!("Publish order ({} crates):", order.len());
    for (i, name) in order.iter().enumerate() {
        println!("  {pos:>2}. {name}", pos = i + 1);
    }
    println!();

    let ext = KitharaExt::from_ctx(ctx)?;
    if args.dry_run {
        println!("Mode: dry-run (validate packaging without upload)");
        run_dry_run(&order, args.verify_registry, &ext.publish)?;
    } else {
        let delay = resolve_delay_secs(args, &ext.publish)?;
        println!("Mode: publish ({delay}s delay between crates)");
        println!();
        run_publish(&order, delay, args.no_verify, &ext.publish)?;
    }

    println!();
    println!("All {} crates OK.", order.len());
    Ok(())
}

pub(crate) fn publish_release(ctx: &Ctx) -> Result<()> {
    run(
        &PublishArgs {
            dry_run: false,
            verify_registry: false,
            delay: None,
            no_verify: true,
        },
        ctx,
    )
}

/// Every crate a release publishes, by name, once each of them is at
/// `version`. A release names one version, and a crate left behind at another
/// would reach the registry under a number nobody released.
pub(crate) fn release_crates(version: &str) -> Result<Vec<String>> {
    let order = resolve_publish_order()?;
    crates_at_version(locate_versions(&order)?, version)
}

/// The crates among `crates` that crates.io already holds at `version`. The
/// registry never takes a version back, so a release any of them reached is
/// final.
pub(crate) fn registered(ctx: &Ctx, crates: &[String], version: &str) -> Result<Vec<String>> {
    let publish = KitharaExt::from_ctx(ctx)?.publish;
    let http_timeout_secs = resolve_http_timeout_secs(&publish)?;
    let mut held = Vec::new();
    for name in crates {
        if registry_has(name, Some(version), &publish.user_agent, http_timeout_secs)? {
            held.push(name.clone());
        }
    }
    Ok(held)
}

fn crates_at_version(versions: HashMap<String, String>, version: &str) -> Result<Vec<String>> {
    let mut behind: Vec<_> = versions
        .iter()
        .filter(|(_, found)| found.as_str() != version)
        .map(|(name, found)| format!("{name} {found}"))
        .collect();
    if !behind.is_empty() {
        behind.sort();
        bail!(
            "the release names {version}, but these crates carry another version: {}",
            behind.join(", ")
        );
    }
    let mut names: Vec<_> = versions.into_keys().collect();
    names.sort();
    Ok(names)
}

/// Dry-run: disable hakari, validate packaging for each crate, re-enable hakari.
///
/// Uses `cargo package --list` to verify that each crate can be packaged
/// (correct metadata, included files, license). Does not resolve deps from
/// crates.io, so it works even when workspace deps are not yet published.
fn run_dry_run(order: &[String], verify_registry: bool, publish: &PublishConfig) -> Result<()> {
    println!("  Disabling hakari workspace-hack...");
    run_cargo(&["hakari", "disable"], "cargo hakari disable")?;

    let result = run_dry_run_inner(order);

    println!("  Re-enabling hakari workspace-hack...");
    run_cargo(&["hakari", "generate"], "cargo hakari generate")?;

    result?;

    if verify_registry {
        run_registry_dry_run(order, publish)?;
    }

    Ok(())
}

fn run_dry_run_inner(order: &[String]) -> Result<()> {
    println!();
    for (i, name) in order.iter().enumerate() {
        let pos = i + 1;
        let total = order.len();
        print!("[{pos}/{total}] Packaging {name}... ");

        let output = Command::new("cargo")
            .args(["package", "-p", name, "--list", "--allow-dirty"])
            .output()
            .with_context(|| format!("failed to run cargo package --list for {name}"))?;

        if !output.status.success() {
            println!("FAILED");
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("cargo package --list failed for {name}:\n{stderr}");
        }

        let file_count = output.stdout.iter().filter(|&&b| b == b'\n').count();
        println!("ok ({file_count} files)");
    }
    Ok(())
}

/// Publish each crate with a delay between them.
///
/// `cargo hakari publish` only strips `kithara-workspace-hack` from the default
/// `[dependencies]` section, leaving it in target-specific sections like
/// `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`. Since kithara
/// places the hack under such a target section, we strip it ourselves and call
/// `cargo publish` directly; the original Cargo.toml is restored unconditionally.
fn run_publish(
    order: &[String],
    delay: u64,
    no_verify: bool,
    publish: &PublishConfig,
) -> Result<()> {
    let inputs = locate_publish_inputs(order)?;
    let versions = locate_versions(order)?;
    let http_timeout_secs = resolve_http_timeout_secs(publish)?;
    let mut pace = NewCratePace::from_config(publish)?;

    let mut published_count = 0usize;
    let mut last_action_was_publish = false;

    for (i, name) in order.iter().enumerate() {
        let pos = i + 1;
        let total = order.len();
        let version = &versions[name];

        if registry_has(name, Some(version), &publish.user_agent, http_timeout_secs)? {
            println!("[{pos}/{total}] Skipping {name} v{version} (already on crates.io).");
            last_action_was_publish = false;
            continue;
        }

        let new_crate = !registry_has(name, None, &publish.user_agent, http_timeout_secs)?;

        if last_action_was_publish && delay > 0 {
            println!("  Waiting {delay}s before next publish...");
            thread::sleep(Duration::from_secs(delay));
        }
        if new_crate {
            let wait = pace.wait(Instant::now());
            if !wait.is_zero() {
                println!(
                    "  {name} is a new crate; waiting {}s for crates.io to accept another...",
                    wait.as_secs()
                );
                thread::sleep(wait);
            }
        }

        println!("[{pos}/{total}] Publishing {name} v{version}...");
        publish_one(
            name,
            &inputs[name],
            &publish.workspace_hack_crate,
            PublishMode::Upload,
            no_verify,
        )?;
        if new_crate {
            pace.record(Instant::now());
        }
        published_count += 1;
        last_action_was_publish = true;
    }

    println!();
    println!(
        "Published {published_count} crate(s); {} already on crates.io.",
        order.len() - published_count
    );
    Ok(())
}

fn run_registry_dry_run(order: &[String], publish: &PublishConfig) -> Result<()> {
    let inputs = locate_publish_inputs(order)?;
    let versions = locate_versions(order)?;
    let deps = locate_publishable_workspace_deps(order)?;
    let http_timeout_secs = resolve_http_timeout_secs(publish)?;

    println!();
    println!("Registry dry-run (cargo publish --dry-run where dependency state allows):");

    for (i, name) in order.iter().enumerate() {
        let pos = i + 1;
        let total = order.len();
        let version = &versions[name];

        if registry_has(name, Some(version), &publish.user_agent, http_timeout_secs)? {
            println!("[{pos}/{total}] Skipping {name} v{version} (already on crates.io).");
            continue;
        }

        let missing_deps = deps
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|dep| {
                let dep_version = &versions[dep];
                match registry_has(
                    dep,
                    Some(dep_version),
                    &publish.user_agent,
                    http_timeout_secs,
                ) {
                    Ok(true) => None,
                    Ok(false) => Some(Ok(format!("{dep} v{dep_version}"))),
                    Err(err) => Some(Err(err)),
                }
            })
            .collect::<Result<Vec<_>>>()?;

        if !missing_deps.is_empty() {
            println!(
                "[{pos}/{total}] Skipping registry dry-run for {name} v{version} \
                 (workspace deps not on crates.io yet: {}).",
                missing_deps.join(", ")
            );
            continue;
        }

        println!("[{pos}/{total}] Registry dry-run for {name} v{version}...");
        publish_one(
            name,
            &inputs[name],
            &publish.workspace_hack_crate,
            PublishMode::DryRun,
            false,
        )?;
    }

    Ok(())
}

/// The pace crates.io keeps one uploader to for new crates: a burst accepted at
/// once, then one each interval. Versions of registered crates are limited
/// separately and far more loosely, so they do not advance it.
#[derive(Debug)]
struct NewCratePace {
    burst: usize,
    interval: Duration,
    registered: usize,
    last: Option<Instant>,
}

impl NewCratePace {
    fn from_config(publish: &PublishConfig) -> Result<Self> {
        let burst = publish.new_crate_burst.context(
            "ext.publish.new_crate_burst is not set; fill in the [ext.publish] section of .config/xtask.toml",
        )?;
        let interval = publish.new_crate_interval_secs.context(
            "ext.publish.new_crate_interval_secs is not set; fill in the [ext.publish] section of .config/xtask.toml",
        )?;
        Ok(Self {
            burst,
            interval: Duration::from_secs(interval),
            registered: 0,
            last: None,
        })
    }

    /// How long a new crate has to wait at `now` before crates.io takes it.
    fn wait(&self, now: Instant) -> Duration {
        match self.last {
            Some(last) if self.registered >= self.burst => {
                (last + self.interval).saturating_duration_since(now)
            }
            _ => Duration::ZERO,
        }
    }

    fn record(&mut self, at: Instant) {
        self.registered += 1;
        self.last = Some(at);
    }
}

fn resolve_delay_secs(args: &PublishArgs, publish: &PublishConfig) -> Result<u64> {
    args.delay.or(publish.delay_secs).context(
        "ext.publish.delay_secs is not set; fill in the [ext.publish] section of .config/xtask.toml",
    )
}

fn resolve_http_timeout_secs(publish: &PublishConfig) -> Result<u64> {
    publish.http_timeout_secs.context(
        "ext.publish.http_timeout_secs is not set; fill in the [ext.publish] section of .config/xtask.toml",
    )
}

fn locate_versions(order: &[String]) -> Result<HashMap<String, String>> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to run cargo metadata")?;
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().collect();
    let wanted: HashSet<&str> = order.iter().map(String::as_str).collect();

    let mut out = HashMap::new();
    for pkg in &metadata.packages {
        if !workspace_members.contains(&pkg.id) {
            continue;
        }
        if !wanted.contains(pkg.name.as_str()) {
            continue;
        }
        out.insert(pkg.name.to_string(), pkg.version.to_string());
    }
    Ok(out)
}

/// HEAD-equivalent check via curl: GET .../api/v1/crates/<name>/<version>,
/// or .../api/v1/crates/<name> for the crate at any version when `version` is
/// `None`. Returns true if status is 200. Any non-2xx/non-404 is reported as
/// an error so transient failures don't silently lead to duplicate-publish
/// attempts.
fn registry_has(
    name: &str,
    version: Option<&str>,
    configured_agent: &str,
    http_timeout_secs: u64,
) -> Result<bool> {
    let path = version.map_or_else(|| name.to_string(), |version| format!("{name}/{version}"));
    let url = format!("https://crates.io/api/v1/crates/{path}");
    let user_agent = if configured_agent.is_empty() {
        Consts::DEFAULT_USER_AGENT
    } else {
        configured_agent
    };
    let timeout = http_timeout_secs.to_string();
    let output = Command::new("curl")
        .args([
            "-sS",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "-A",
            user_agent,
            "--max-time",
            &timeout,
            &url,
        ])
        .output()
        .with_context(|| format!("curl crates.io for {path}"))?;
    if !output.status.success() {
        bail!(
            "curl failed for {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let code = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match code.as_str() {
        "200" => Ok(true),
        "404" => Ok(false),
        other => bail!(
            "unexpected HTTP {other} from crates.io for {path} \
             (refusing to proceed; check network and retry)"
        ),
    }
}

/// What `cargo publish` of one crate needs beyond its name: the manifest to
/// rewrite and the manifest keys of its workspace dev-dependencies.
struct PublishInput {
    manifest: PathBuf,
    workspace_dev_deps: Vec<String>,
}

fn locate_publish_inputs(order: &[String]) -> Result<HashMap<String, PublishInput>> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to run cargo metadata")?;

    let workspace_members: HashSet<_> = metadata.workspace_members.iter().collect();
    let wanted: HashSet<&str> = order.iter().map(String::as_str).collect();

    let mut out = HashMap::new();
    for pkg in &metadata.packages {
        if !workspace_members.contains(&pkg.id) {
            continue;
        }
        if !wanted.contains(pkg.name.as_str()) {
            continue;
        }
        let workspace_dev_deps: BTreeSet<String> = pkg
            .dependencies
            .iter()
            .filter(|dep| dep.path.is_some() && dep.kind == DependencyKind::Development)
            .map(|dep| dep.rename.clone().unwrap_or_else(|| dep.name.clone()))
            .collect();
        out.insert(
            pkg.name.to_string(),
            PublishInput {
                manifest: PathBuf::from(pkg.manifest_path.as_str()),
                workspace_dev_deps: workspace_dev_deps.into_iter().collect(),
            },
        );
    }

    for name in order {
        if !out.contains_key(name) {
            bail!("manifest not found for crate {name}");
        }
    }
    Ok(out)
}

fn locate_publishable_workspace_deps(order: &[String]) -> Result<HashMap<String, Vec<String>>> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to run cargo metadata")?;
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().collect();
    let wanted: HashSet<&str> = order.iter().map(String::as_str).collect();

    let mut out = HashMap::new();
    for pkg in &metadata.packages {
        if !workspace_members.contains(&pkg.id) {
            continue;
        }
        if !wanted.contains(pkg.name.as_str()) {
            continue;
        }

        let deps: BTreeSet<_> = pkg
            .dependencies
            .iter()
            .filter(|dep| dep.path.is_some() && dep.kind != DependencyKind::Development)
            .map(|dep| dep.name.clone())
            .filter(|dep_name| wanted.contains(dep_name.as_str()) && dep_name != pkg.name.as_str())
            .collect();
        out.insert(pkg.name.to_string(), deps.into_iter().collect());
    }

    Ok(out)
}

#[derive(Clone, Copy)]
enum PublishMode {
    DryRun,
    Upload,
}

impl PublishMode {
    fn cargo_args(self, name: &str, no_verify: bool) -> Vec<&str> {
        let mut args = vec!["publish", "-p", name, "--allow-dirty"];
        if matches!(self, Self::DryRun) {
            args.push("--dry-run");
        }
        if no_verify {
            args.push("--no-verify");
        }
        args
    }

    fn description(self, name: &str) -> String {
        match self {
            Self::DryRun => format!("cargo publish --dry-run -p {name}"),
            Self::Upload => format!("cargo publish -p {name}"),
        }
    }
}

fn publish_one(
    name: &str,
    input: &PublishInput,
    hack_crate: &str,
    mode: PublishMode,
    no_verify: bool,
) -> Result<()> {
    let manifest = &input.manifest;
    let original = fs::read_to_string(manifest)
        .with_context(|| format!("read {} for {name}", manifest.display()))?;
    let stripped = publish_manifest(&original, hack_crate, &input.workspace_dev_deps)
        .with_context(|| format!("rewrite {} for {name}", manifest.display()))?;
    let did_strip = stripped.is_some();
    let lockfile = PublishLockfile::snapshot()?;

    if let Some(stripped) = &stripped {
        fs::write(manifest, stripped)
            .with_context(|| format!("write stripped manifest {}", manifest.display()))?;
        println!("  Temporarily removed {hack_crate} and workspace dev-dependencies.");
    }

    let args = mode.cargo_args(name, no_verify);
    let result = run_cargo(&args, &mode.description(name));

    let restore_result = restore_publish_inputs(manifest, &original, did_strip, lockfile);
    if let Err(restore_err) = restore_result {
        return match result {
            Ok(()) => Err(restore_err),
            Err(run_err) => Err(run_err)
                .with_context(|| format!("also failed to restore publish inputs: {restore_err}")),
        };
    }

    result
}

fn restore_publish_inputs(
    manifest: &Path,
    original_manifest: &str,
    did_strip: bool,
    lockfile: PublishLockfile,
) -> Result<()> {
    let mut errors = Vec::new();

    if did_strip && let Err(err) = fs::write(manifest, original_manifest) {
        errors.push(format!("restore {}: {err}", manifest.display()));
    }

    if let Err(err) = lockfile.restore() {
        errors.push(format!("restore Cargo.lock: {err}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        bail!("{}", errors.join("; "))
    }
}

struct PublishLockfile {
    path: PathBuf,
    contents: Option<String>,
}

impl PublishLockfile {
    fn snapshot() -> Result<Self> {
        let path = PathBuf::from("Cargo.lock");
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => Some(contents),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        Ok(Self { path, contents })
    }

    fn restore(self) -> Result<()> {
        match self.contents {
            Some(contents) => fs::write(&self.path, contents)
                .with_context(|| format!("restore {}", self.path.display())),
            None => match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err).with_context(|| format!("remove {}", self.path.display())),
            },
        }
    }
}

/// Remove every `kithara-workspace-hack = { ... }` dependency line, regardless
/// of whether it lives under `[dependencies]` or a `[target.<cfg>.dependencies]`
/// section. Preserves all other content byte-for-byte.
/// The manifest `cargo publish` sees, or `None` when it needs no change.
///
/// `cargo hakari publish` only strips the workspace-hack from the default
/// `[dependencies]` table, and kithara places it under a target table, so it is
/// removed from every dependency table here. Workspace dev-dependencies carry a
/// version, so cargo would resolve each against crates.io before its turn in
/// the publish order; they are removed as cargo removes version-less path
/// dev-dependencies, which keeps the order free of dev-dependency cycles.
fn publish_manifest(
    manifest: &str,
    hack_crate: &str,
    workspace_dev_deps: &[String],
) -> Result<Option<String>> {
    let mut table: toml::Table = manifest.parse().context("parse manifest")?;
    let mut removed = strip_dependency_tables(&mut table, hack_crate, workspace_dev_deps);
    if let Some(toml::Value::Table(targets)) = table.get_mut("target") {
        for (_, target) in targets.iter_mut() {
            if let toml::Value::Table(target) = target {
                removed |= strip_dependency_tables(target, hack_crate, workspace_dev_deps);
            }
        }
    }
    if let Some(toml::Value::Table(features)) = table.get_mut("features") {
        removed |= strip_feature_references(features, hack_crate);
    }
    if !removed {
        return Ok(None);
    }
    toml::to_string(&table)
        .map(Some)
        .context("serialize publish manifest")
}

fn strip_dependency_tables(
    table: &mut toml::Table,
    hack_crate: &str,
    workspace_dev_deps: &[String],
) -> bool {
    let mut removed = false;
    for (section, names) in [
        ("dependencies", std::slice::from_ref(&hack_crate.to_owned())),
        ("dev-dependencies", workspace_dev_deps),
    ] {
        if let Some(toml::Value::Table(deps)) = table.get_mut(section) {
            for name in names {
                removed |= deps.remove(name).is_some();
            }
        }
    }
    removed
}

/// Cargo rejects a manifest whose feature names a dependency it does not list,
/// so a feature that enabled the removed workspace-hack keeps its name and
/// loses that entry.
fn strip_feature_references(features: &mut toml::Table, dependency: &str) -> bool {
    let names_dependency = |entry: &str| {
        let entry = entry.strip_prefix("dep:").unwrap_or(entry);
        entry
            .strip_prefix(dependency)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with("?/"))
    };
    let mut removed = false;
    for (_, enabled) in features.iter_mut() {
        if let toml::Value::Array(enabled) = enabled {
            let before = enabled.len();
            enabled.retain(|entry| entry.as_str().is_none_or(|entry| !names_dependency(entry)));
            removed |= enabled.len() != before;
        }
    }
    removed
}

fn run_cargo(args: &[&str], description: &str) -> Result<()> {
    let status = Command::new("cargo")
        .args(args)
        .status()
        .with_context(|| format!("failed to run {description}"))?;
    if !status.success() {
        bail!(
            "{description} failed (exit code: {})",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

/// Resolve the topological publish order from workspace metadata.
///
/// Returns crate names sorted so that dependencies come before dependents.
/// Crates with `publish = false` are excluded.
fn resolve_publish_order() -> Result<Vec<String>> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to run cargo metadata")?;

    let workspace_members: HashSet<_> = metadata.workspace_members.iter().collect();

    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut all_publishable: HashSet<String> = HashSet::new();

    for pkg in &metadata.packages {
        if !workspace_members.contains(&pkg.id) {
            continue;
        }

        if matches!(&pkg.publish, Some(registries) if registries.is_empty()) {
            continue;
        }

        all_publishable.insert(pkg.name.to_string());
        graph.insert(pkg.name.to_string(), Vec::new());
    }

    for pkg in &metadata.packages {
        let name = pkg.name.to_string();
        if !all_publishable.contains(&name) {
            continue;
        }

        let deps: Vec<String> = pkg
            .dependencies
            .iter()
            .filter(|dep| dep.path.is_some() && dep.kind != DependencyKind::Development)
            .map(|dep| dep.name.clone())
            .filter(|dep_name| all_publishable.contains(dep_name) && *dep_name != name)
            .collect();

        graph.insert(name, deps);
    }

    topo_sort(&graph)
}

/// Kahn's algorithm: returns names in publish order (dependencies first).
fn topo_sort(graph: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for name in graph.keys() {
        in_degree.entry(name).or_insert(0);
        dependents.entry(name).or_default();
    }

    for (dependent, deps) in graph {
        for dep in deps {
            dependents.entry(dep).or_default().push(dependent);
            *in_degree.entry(dependent).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&str> = {
        let mut roots: Vec<&str> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(name, _)| *name)
            .collect();
        roots.sort_unstable();
        roots.into()
    };

    let mut order = Vec::with_capacity(graph.len());

    while let Some(name) = queue.pop_front() {
        order.push(name.to_string());

        if let Some(deps) = dependents.get(name) {
            let mut ready: Vec<&str> = deps
                .iter()
                .filter(|dep| {
                    let deg = in_degree
                        .get_mut(*dep)
                        .expect("dependency must be in in_degree map");
                    *deg -= 1;
                    *deg == 0
                })
                .copied()
                .collect();
            ready.sort_unstable();
            queue.extend(ready);
        }
    }

    if order.len() != graph.len() {
        let published: HashSet<&str> = order.iter().map(String::as_str).collect();
        let remaining: Vec<_> = graph
            .keys()
            .filter(|k| !published.contains(k.as_str()))
            .collect();
        bail!(
            "Cyclic dependency detected among publishable crates: {:?}",
            remaining
        );
    }

    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_refuses_a_crate_at_another_version() {
        let versions = HashMap::from([
            ("kithara".to_string(), "0.0.2".to_string()),
            ("kithara-net".to_string(), "0.0.1".to_string()),
        ]);

        let error = crates_at_version(versions, "0.0.2").unwrap_err();

        assert!(error.to_string().contains("kithara-net 0.0.1"), "{error}");
    }

    #[test]
    fn a_release_lists_its_crates_by_name() {
        let versions = HashMap::from([
            ("kithara-net".to_string(), "0.0.2".to_string()),
            ("kithara".to_string(), "0.0.2".to_string()),
        ]);

        let names = crates_at_version(versions, "0.0.2").unwrap();

        assert_eq!(names, ["kithara", "kithara-net"]);
    }

    #[test]
    fn publish_order_is_resolved() {
        let order = resolve_publish_order().unwrap();
        assert!(!order.is_empty(), "should find publishable crates");

        let metadata = MetadataCommand::new().exec().unwrap();
        let members: HashSet<_> = metadata.workspace_members.iter().collect();
        let publishable: HashSet<String> = metadata
            .packages
            .iter()
            .filter(|pkg| members.contains(&pkg.id))
            .filter(|pkg| !matches!(&pkg.publish, Some(registries) if registries.is_empty()))
            .map(|pkg| pkg.name.to_string())
            .collect();

        let ordered: HashSet<String> = order.iter().cloned().collect();
        assert_eq!(
            ordered.len(),
            order.len(),
            "publish order must not repeat crates: {order:?}"
        );
        assert_eq!(
            ordered, publishable,
            "publish order must cover exactly the publishable workspace members"
        );

        let position: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(idx, name)| (name.as_str(), idx))
            .collect();
        for pkg in &metadata.packages {
            let name = pkg.name.to_string();
            if !publishable.contains(&name) {
                continue;
            }
            for dep in &pkg.dependencies {
                let dep_name = dep.name.clone();
                if dep.path.is_none()
                    || dep.kind == DependencyKind::Development
                    || dep_name == name
                    || !publishable.contains(&dep_name)
                {
                    continue;
                }
                assert!(
                    position[dep_name.as_str()] < position[name.as_str()],
                    "{dep_name} must be published before its dependent {name}"
                );
            }
        }
    }

    #[test]
    fn publish_order_excludes_non_publishable() {
        let order = resolve_publish_order().unwrap();
        let names: HashSet<_> = order.iter().map(String::as_str).collect();
        assert!(!names.contains("kithara-workspace-hack"));
        assert!(!names.contains("kithara-app"));
        assert!(!names.contains("xtask"));
    }

    #[test]
    fn topo_sort_simple_graph() {
        let mut graph = HashMap::new();
        graph.insert("c".to_string(), vec!["b".to_string(), "a".to_string()]);
        graph.insert("b".to_string(), vec!["a".to_string()]);
        graph.insert("a".to_string(), vec![]);

        let order = topo_sort(&graph).unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn publish_manifest_strips_the_hack_and_workspace_dev_deps_from_every_table() {
        let input = "\
[dependencies]
foo = { workspace = true }
kithara-workspace-hack = { version = \"0.0.1-alpha1\", path = \"../kithara-workspace-hack\" }

[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]
kithara-workspace-hack = { version = \"0.0.1-alpha1\", path = \"../kithara-workspace-hack\" }
bar = { workspace = true }

[dev-dependencies]
kithara-test-utils = { workspace = true }
serde = { workspace = true }

[target.'cfg(target_os = \"android\")'.dev-dependencies]
kithara-test-dylib = { path = \"../dylib\", features = [
    \"a\",
] }
";
        let dev_deps = [
            "kithara-test-dylib".to_owned(),
            "kithara-test-utils".to_owned(),
        ];

        let out = publish_manifest(input, "kithara-workspace-hack", &dev_deps)
            .unwrap()
            .unwrap();

        let table: toml::Table = out.parse().unwrap();
        let text = table.to_string();
        for gone in [
            "kithara-workspace-hack",
            "kithara-test-utils",
            "kithara-test-dylib",
        ] {
            assert!(!text.contains(gone), "{gone} survived:\n{text}");
        }
        assert!(table["dependencies"].get("foo").is_some(), "{text}");
        assert!(table["dev-dependencies"].get("serde").is_some(), "{text}");
        assert!(
            table["target"]["cfg(not(target_arch = \"wasm32\"))"]["dependencies"]
                .get("bar")
                .is_some(),
            "{text}"
        );
    }

    #[test]
    fn publish_manifest_leaves_a_manifest_without_workspace_deps_alone() {
        let input = "[dependencies]\nserde = \"1\"\n";
        assert_eq!(
            publish_manifest(input, "kithara-workspace-hack", &[]).unwrap(),
            None
        );
    }

    #[test]
    fn every_published_manifest_names_only_workspace_crates_published_before_it() {
        let metadata = MetadataCommand::new().exec().unwrap();
        let members: HashSet<_> = metadata.workspace_members.iter().collect();
        let workspace: HashSet<String> = metadata
            .packages
            .iter()
            .filter(|pkg| members.contains(&pkg.id))
            .map(|pkg| pkg.name.to_string())
            .collect();
        let order = resolve_publish_order().unwrap();
        let position: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, name)| (name.as_str(), i))
            .collect();
        let inputs = locate_publish_inputs(&order).unwrap();

        for name in &order {
            let input = &inputs[name];
            let original = fs::read_to_string(&input.manifest).unwrap();
            let published = publish_manifest(
                &original,
                "kithara-workspace-hack",
                &input.workspace_dev_deps,
            )
            .unwrap()
            .unwrap_or(original);
            let table: toml::Table = published.parse().unwrap();

            for dep in workspace_deps_named_in(&table, &workspace) {
                let Some(&at) = position.get(dep.as_str()) else {
                    panic!("{name} publishes naming unpublished workspace crate {dep}");
                };
                assert!(
                    at < position[name.as_str()],
                    "{name} publishes naming {dep}, which is published after it"
                );
            }
            let missing = features_naming_absent_deps(&table);
            assert!(
                missing.is_empty(),
                "{name} publishes features naming {missing:?}, which it does not depend on"
            );
        }
    }

    #[test]
    fn publish_manifest_keeps_a_feature_that_enabled_the_hack() {
        let input = "\
[features]
workspace-hack = [\"dep:kithara-workspace-hack\"]
full = [\"workspace-hack\", \"kithara-workspace-hack?/std\", \"foo/std\"]

[dependencies]
foo = { workspace = true }

[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]
kithara-workspace-hack = { version = \"0.0.1-alpha1\", path = \"../kithara-workspace-hack\", optional = true }
";
        let out = publish_manifest(input, "kithara-workspace-hack", &[])
            .unwrap()
            .unwrap();

        let table: toml::Table = out.parse().unwrap();
        assert_eq!(
            table["features"]["workspace-hack"],
            toml::Value::Array(vec![])
        );
        assert_eq!(
            table["features"]["full"],
            toml::Value::Array(vec!["workspace-hack".into(), "foo/std".into()])
        );
        assert_eq!(features_naming_absent_deps(&table), Vec::<String>::new());
    }

    /// The dependencies feature entries name that the manifest does not list,
    /// which cargo refuses to parse.
    fn features_naming_absent_deps(table: &toml::Table) -> Vec<String> {
        let mut tables = vec![table];
        if let Some(toml::Value::Table(targets)) = table.get("target") {
            tables.extend(targets.values().filter_map(toml::Value::as_table));
        }
        let listed: HashSet<&str> = tables
            .iter()
            .filter_map(|table| table.get("dependencies").and_then(toml::Value::as_table))
            .flat_map(|deps| deps.keys().map(String::as_str))
            .collect();
        let Some(features) = table.get("features").and_then(toml::Value::as_table) else {
            return Vec::new();
        };
        features
            .values()
            .filter_map(toml::Value::as_array)
            .flatten()
            .filter_map(toml::Value::as_str)
            .filter_map(|entry| {
                let named = match entry.strip_prefix("dep:") {
                    Some(dep) => dep,
                    None => entry.split_once('/')?.0.trim_end_matches('?'),
                };
                (!listed.contains(named)).then(|| named.to_owned())
            })
            .collect()
    }

    fn workspace_deps_named_in(table: &toml::Table, workspace: &HashSet<String>) -> Vec<String> {
        let mut tables = vec![table];
        if let Some(toml::Value::Table(targets)) = table.get("target") {
            tables.extend(targets.values().filter_map(toml::Value::as_table));
        }
        let mut named = Vec::new();
        for table in tables {
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                let Some(toml::Value::Table(deps)) = table.get(section) else {
                    continue;
                };
                for (key, spec) in deps {
                    let package = spec
                        .get("package")
                        .and_then(toml::Value::as_str)
                        .unwrap_or(key);
                    if workspace.contains(package) {
                        named.push(package.to_owned());
                    }
                }
            }
        }
        named
    }

    #[test]
    fn publish_delay_uses_config_when_cli_arg_is_absent() {
        let args = PublishArgs {
            dry_run: false,
            verify_registry: false,
            delay: None,
            no_verify: false,
        };
        let publish = PublishConfig {
            workspace_hack_crate: String::new(),
            delay_secs: Some(20),
            http_timeout_secs: Some(20),
            user_agent: String::new(),
            ..PublishConfig::default()
        };

        assert_eq!(resolve_delay_secs(&args, &publish).unwrap(), 20);
    }

    #[test]
    fn publish_delay_cli_arg_overrides_config() {
        let args = PublishArgs {
            dry_run: false,
            verify_registry: false,
            delay: Some(610),
            no_verify: false,
        };
        let publish = PublishConfig {
            workspace_hack_crate: String::new(),
            delay_secs: Some(20),
            http_timeout_secs: Some(20),
            user_agent: String::new(),
            ..PublishConfig::default()
        };

        assert_eq!(resolve_delay_secs(&args, &publish).unwrap(), 610);
    }

    #[test]
    fn publish_delay_requires_config_when_cli_arg_is_absent() {
        let args = PublishArgs {
            dry_run: false,
            verify_registry: false,
            delay: None,
            no_verify: false,
        };
        let publish = PublishConfig {
            workspace_hack_crate: String::new(),
            delay_secs: None,
            http_timeout_secs: Some(20),
            user_agent: String::new(),
            ..PublishConfig::default()
        };

        let error = resolve_delay_secs(&args, &publish).unwrap_err();
        assert!(error.to_string().contains("delay_secs"), "{error}");
    }

    #[test]
    fn publish_http_timeout_uses_config() {
        let publish = PublishConfig {
            workspace_hack_crate: String::new(),
            delay_secs: Some(20),
            http_timeout_secs: Some(20),
            user_agent: String::new(),
            ..PublishConfig::default()
        };

        assert_eq!(resolve_http_timeout_secs(&publish).unwrap(), 20);
    }

    #[test]
    fn publish_http_timeout_requires_config() {
        let publish = PublishConfig {
            workspace_hack_crate: String::new(),
            delay_secs: Some(20),
            http_timeout_secs: None,
            user_agent: String::new(),
            ..PublishConfig::default()
        };

        let error = resolve_http_timeout_secs(&publish).unwrap_err();
        assert!(error.to_string().contains("http_timeout_secs"), "{error}");
    }

    #[test]
    fn topo_sort_detects_cycle() {
        let mut graph = HashMap::new();
        graph.insert("a".to_string(), vec!["b".to_string()]);
        graph.insert("b".to_string(), vec!["a".to_string()]);

        let result = topo_sort(&graph);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Cyclic dependency"), "{msg}");
    }

    /// Once the burst is spent, each new crate waits out the interval from the
    /// last one registered, however long the run spent in between.
    #[test]
    fn new_crates_past_the_burst_keep_the_registry_pace() {
        let publish = PublishConfig {
            new_crate_burst: Some(2),
            new_crate_interval_secs: Some(600),
            ..PublishConfig::default()
        };
        let mut pace = NewCratePace::from_config(&publish).unwrap();
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);

        assert_eq!(pace.wait(start), Duration::ZERO);
        pace.record(start);
        assert_eq!(pace.wait(at(5)), Duration::ZERO);
        pace.record(at(10));

        assert_eq!(pace.wait(at(10)), Duration::from_secs(600));
        assert_eq!(pace.wait(at(400)), Duration::from_secs(210));
        assert_eq!(pace.wait(at(610)), Duration::ZERO);
        pace.record(at(700));
        assert_eq!(pace.wait(at(700)), Duration::from_secs(600));
    }

    #[test]
    fn new_crate_pace_requires_config() {
        let error = NewCratePace::from_config(&PublishConfig::default()).unwrap_err();
        assert!(error.to_string().contains("new_crate_burst"), "{error}");
    }
}
