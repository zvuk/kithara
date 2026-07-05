use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use cargo_metadata::{DependencyKind, MetadataCommand};
use kithara_devtools::{Ctx, util::check_tool};

use crate::config::{KitharaExt, PublishConfig};

struct Consts;

impl Consts {
    const DEFAULT_DELAY_SECS: u64 = 20;
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

    /// Delay in seconds between publishes [default: 20].
    /// Skipped during dry-run. For first-time publishing of new crates,
    /// use 610 (crates.io allows 5 new crates burst, then 1 per 10 min).
    #[arg(long, default_value_t = Consts::DEFAULT_DELAY_SECS)]
    delay: u64,

    /// Skip the verification build (`cargo publish --no-verify`). Required for
    /// workspace library crates that leave the HTTP backend to the consumer:
    /// an isolated default-feature build selects no backend and fails to
    /// compile even though the packaged source is correct.
    #[arg(long)]
    no_verify: bool,
}

pub(crate) fn run(args: &PublishArgs, ctx: &Ctx) -> Result<()> {
    check_tool(
        "cargo-hakari",
        &["hakari", "--version"],
        "cargo install cargo-hakari",
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
        println!("Mode: publish ({}s delay between crates)", args.delay);
        println!();
        run_publish(&order, args.delay, args.no_verify, &ext.publish)?;
    }

    println!();
    println!("All {} crates OK.", order.len());
    Ok(())
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
    let manifests = locate_manifests(order)?;
    let versions = locate_versions(order)?;

    let mut published_count = 0usize;
    let mut last_action_was_publish = false;

    for (i, name) in order.iter().enumerate() {
        let pos = i + 1;
        let total = order.len();
        let version = &versions[name];

        if registry_has(name, version, &publish.user_agent)? {
            println!("[{pos}/{total}] Skipping {name} v{version} (already on crates.io).");
            last_action_was_publish = false;
            continue;
        }

        if last_action_was_publish && delay > 0 {
            println!("  Waiting {delay}s before next publish...");
            thread::sleep(Duration::from_secs(delay));
        }

        println!("[{pos}/{total}] Publishing {name} v{version}...");
        let manifest = &manifests[name];
        publish_one(
            name,
            manifest,
            &publish.workspace_hack_crate,
            PublishMode::Upload,
            no_verify,
        )?;
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
    let manifests = locate_manifests(order)?;
    let versions = locate_versions(order)?;
    let deps = locate_publishable_workspace_deps(order)?;

    println!();
    println!("Registry dry-run (cargo publish --dry-run where dependency state allows):");

    for (i, name) in order.iter().enumerate() {
        let pos = i + 1;
        let total = order.len();
        let version = &versions[name];

        if registry_has(name, version, &publish.user_agent)? {
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
                match registry_has(dep, dep_version, &publish.user_agent) {
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
            &manifests[name],
            &publish.workspace_hack_crate,
            PublishMode::DryRun,
            false,
        )?;
    }

    Ok(())
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

/// HEAD-equivalent check via curl: GET .../api/v1/crates/<name>/<version>.
/// Returns true if status is 200. Any non-2xx/non-404 is reported as an
/// error so transient failures don't silently lead to duplicate-publish
/// attempts.
fn registry_has(name: &str, version: &str, configured_agent: &str) -> Result<bool> {
    let url = format!("https://crates.io/api/v1/crates/{name}/{version}");
    let user_agent = if configured_agent.is_empty() {
        Consts::DEFAULT_USER_AGENT
    } else {
        configured_agent
    };
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
            "20",
            &url,
        ])
        .output()
        .with_context(|| format!("curl crates.io for {name} {version}"))?;
    if !output.status.success() {
        bail!(
            "curl failed for {name} {version}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let code = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match code.as_str() {
        "200" => Ok(true),
        "404" => Ok(false),
        other => bail!(
            "unexpected HTTP {other} from crates.io for {name} {version} \
             (refusing to proceed; check network and retry)"
        ),
    }
}

fn locate_manifests(order: &[String]) -> Result<HashMap<String, PathBuf>> {
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
        out.insert(
            pkg.name.to_string(),
            PathBuf::from(pkg.manifest_path.as_str()),
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
            .map(|dep| dep.name.to_string())
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
    manifest: &Path,
    hack_crate: &str,
    mode: PublishMode,
    no_verify: bool,
) -> Result<()> {
    let original = fs::read_to_string(manifest)
        .with_context(|| format!("read {} for {name}", manifest.display()))?;
    let stripped = strip_workspace_hack(&original, hack_crate);
    let did_strip = stripped != original;
    let lockfile = PublishLockfile::snapshot()?;

    if did_strip {
        fs::write(manifest, &stripped)
            .with_context(|| format!("write stripped manifest {}", manifest.display()))?;
        println!("  Temporarily removed {hack_crate} dependency.");
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
fn strip_workspace_hack(manifest: &str, hack_crate: &str) -> String {
    if hack_crate.is_empty() {
        return manifest.to_owned();
    }
    let mut out = String::with_capacity(manifest.len());
    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with(hack_crate)
            && trimmed[hack_crate.len()..].trim_start().starts_with('=')
        {
            continue;
        }
        out.push_str(line);
    }
    out
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
            .map(|dep| dep.name.to_string())
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
        roots.sort();
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
            ready.sort();
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
    fn publish_order_is_resolved() {
        let order = resolve_publish_order().unwrap();
        assert!(!order.is_empty(), "should find publishable crates");
        assert_eq!(
            order.len(),
            21,
            "publish order should cover all publishable crates"
        );

        let platform_pos = order.iter().position(|n| n == "kithara-platform");
        let facade_pos = order.iter().position(|n| n == "kithara");
        if let (Some(p), Some(f)) = (platform_pos, facade_pos) {
            assert!(p < f, "kithara-platform must be published before kithara");
        }

        let stretch_pos = order
            .iter()
            .position(|n| n == "kithara-stretch")
            .expect("kithara-stretch should be publishable");
        let audio_pos = order
            .iter()
            .position(|n| n == "kithara-audio")
            .expect("kithara-audio should be publishable");
        assert!(
            stretch_pos < audio_pos,
            "kithara-stretch must be published before kithara-audio"
        );
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
    fn strip_workspace_hack_removes_from_default_and_target_sections() {
        let input = "\
[dependencies]
foo = { workspace = true }
kithara-workspace-hack = { version = \"0.0.1-alpha1\", path = \"../kithara-workspace-hack\" }

[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]
kithara-workspace-hack = { version = \"0.0.1-alpha1\", path = \"../kithara-workspace-hack\" }
bar = { workspace = true }
";
        let out = strip_workspace_hack(input, "kithara-workspace-hack");
        assert!(!out.contains("kithara-workspace-hack"), "{out}");
        assert!(out.contains("foo = { workspace = true }"));
        assert!(out.contains("bar = { workspace = true }"));
        assert!(out.contains("[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]"));
    }

    #[test]
    fn strip_workspace_hack_keeps_unrelated_lines() {
        let input = "no_hack_here = true\n";
        assert_eq!(strip_workspace_hack(input, "kithara-workspace-hack"), input);
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
}
