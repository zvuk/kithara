//! Publish all public workspace crates to crates.io in dependency order.
//!
//! Uses `cargo hakari publish` per crate to temporarily remove the
//! workspace-hack dependency before each publish.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;

use crate::util::check_tool;

const DEFAULT_DELAY_SECS: u64 = 20;

#[derive(Debug, clap::Args)]
pub(crate) struct PublishArgs {
    /// Perform a dry run (pass --dry-run to cargo publish).
    #[arg(long)]
    dry_run: bool,

    /// Delay in seconds between publishes [default: 20].
    /// Skipped during dry-run. For first-time publishing of new crates,
    /// use 610 (crates.io allows 5 new crates burst, then 1 per 10 min).
    #[arg(long, default_value_t = DEFAULT_DELAY_SECS)]
    delay: u64,
}

pub(crate) fn run(args: &PublishArgs) -> Result<()> {
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

    if args.dry_run {
        println!("Mode: dry-run (validate packaging without upload)");
        run_dry_run(&order)?;
    } else {
        println!("Mode: publish ({}s delay between crates)", args.delay);
        println!();
        run_publish(&order, args.delay)?;
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
fn run_dry_run(order: &[String]) -> Result<()> {
    println!("  Disabling hakari workspace-hack...");
    run_cargo(&["hakari", "disable"], "cargo hakari disable")?;

    let result = run_dry_run_inner(order);

    println!("  Re-enabling hakari workspace-hack...");
    run_cargo(&["hakari", "generate"], "cargo hakari generate")?;

    result
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

/// Publish each crate using `cargo hakari publish` with a delay between them.
fn run_publish(order: &[String], delay: u64) -> Result<()> {
    for (i, name) in order.iter().enumerate() {
        let pos = i + 1;
        let total = order.len();
        println!("[{pos}/{total}] Publishing {name}...");

        run_cargo(
            &["hakari", "publish", "-p", name],
            &format!("cargo hakari publish -p {name}"),
        )?;

        let is_last = pos == total;
        if !is_last && delay > 0 {
            println!("  Waiting {delay}s before next publish...");
            thread::sleep(Duration::from_secs(delay));
        }
    }
    Ok(())
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

    // Collect publishable workspace packages and their workspace dependencies.
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut all_publishable: HashSet<String> = HashSet::new();

    for pkg in &metadata.packages {
        if !workspace_members.contains(&pkg.id) {
            continue;
        }

        // `publish` field: None means "publish anywhere", Some([]) means "publish = false".
        if matches!(&pkg.publish, Some(registries) if registries.is_empty()) {
            continue;
        }

        all_publishable.insert(pkg.name.to_string());
        graph.insert(pkg.name.to_string(), Vec::new());
    }

    // Second pass: collect only publishable workspace dependencies.
    for pkg in &metadata.packages {
        let name = pkg.name.to_string();
        if !all_publishable.contains(&name) {
            continue;
        }

        let deps: Vec<String> = pkg
            .dependencies
            .iter()
            .filter(|dep| dep.path.is_some())
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

        // kithara-platform should come before kithara (facade depends on platform).
        let platform_pos = order.iter().position(|n| n == "kithara-platform");
        let facade_pos = order.iter().position(|n| n == "kithara");
        if let (Some(p), Some(f)) = (platform_pos, facade_pos) {
            assert!(p < f, "kithara-platform must be published before kithara");
        }
    }

    #[test]
    fn publish_order_excludes_non_publishable() {
        let order = resolve_publish_order().unwrap();
        let names: HashSet<_> = order.iter().map(|s| s.as_str()).collect();
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
