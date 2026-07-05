use std::{collections::BTreeSet, fs};

use anyhow::{Context as _, Result};
use toml::Value;

use super::{Check, Context};
use crate::common::{scope::packages_in_scope, violation::Violation};

pub(crate) const ID: &str = "tokio_dep_quarantine";

pub(crate) struct TokioDepQuarantine;

impl Check for TokioDepQuarantine {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.tokio_dep_quarantine;
        let exempt: BTreeSet<&str> = cfg.exempt_crates.iter().map(String::as_str).collect();
        let allowed: BTreeSet<&str> = cfg.allowed_crates.iter().map(String::as_str).collect();
        let mut violations = Vec::new();

        for pkg in packages_in_scope(ctx.metadata, ctx.scope) {
            let name = pkg.name.as_str();
            if exempt.contains(name) || allowed.contains(name) {
                continue;
            }
            let manifest = pkg.manifest_path.as_std_path();
            let text = fs::read_to_string(manifest)
                .with_context(|| format!("read manifest: {}", manifest.display()))?;
            let parsed: Value = toml::from_str(&text)
                .with_context(|| format!("parse manifest: {}", manifest.display()))?;

            for dep in production_tokio_deps(&parsed) {
                violations.push(
                    Violation::deny(
                        ID,
                        format!("{name}::{dep}"),
                        format!(
                            "direct production dependency on `{dep}`; only `kithara-platform` \
                             may couple to tokio — depend on its re-exports instead"
                        ),
                    )
                    .with_explanation(EXPLANATION),
                );
            }
        }
        Ok(violations)
    }
}

/// Collect the quarantined crate names that appear as a *production* dependency
/// of this manifest. Scans `[dependencies]` and every
/// `[target.'…'.dependencies]` table; deliberately ignores `[dev-dependencies]`
/// and `[build-dependencies]` (test/build-time tokio — e.g. an axum test
/// server's runtime — is not production coupling). Results are de-duplicated
/// and ordered for stable keys.
fn production_tokio_deps(manifest: &Value) -> Vec<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    collect_from_table(manifest.get("dependencies"), &mut found);
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for spec in targets.values() {
            collect_from_table(spec.get("dependencies"), &mut found);
        }
    }
    found.into_iter().collect()
}

fn collect_from_table(table: Option<&Value>, out: &mut BTreeSet<String>) {
    // Crate names quarantined: a *production* (non-dev, non-build) dependency
    // on any of these is forbidden outside the platform owner / exemptions.
    const QUARANTINED: &[&str] = &["tokio", "tokio-util", "tokio-stream"];
    let Some(table) = table.and_then(Value::as_table) else {
        return;
    };
    for key in table.keys() {
        if QUARANTINED.contains(&key.as_str()) {
            out.insert(key.clone());
        }
    }
}

const EXPLANATION: &str = "\
Summary: a crate other than `kithara-platform` declares a direct *production*
dependency on `tokio` / `tokio-util` / `tokio-stream`. The platform crate is the
single quarantine boundary for the async runtime: it wraps tokio's runtime,
sync, time, and task primitives behind `kithara_platform::{tokio, time, sync}`
so the workspace has one swappable runtime seam (and so wasm builds, which use
`tokio_with_wasm`, stay buildable).

Why: spreading direct tokio deps re-couples every crate to a specific runtime
and to un-virtualizable tokio timers/locks, defeating the flash virtual clock
and the wasm portability layer. reqwest and axum pull tokio *transitively* —
that is fine; the ban is on a *direct, named* tokio dependency in a crate's own
manifest.

Exempt: `kithara-workspace-hack` (the feature-unification shim that must name
every transitive dep) and the test-support crates. Dev- and build-dependencies
are not scanned: a test-only tokio runtime (e.g. an axum fixture server in
`[dev-dependencies]`, as in `kithara-net`) is not production coupling.

Fix: drop the direct dep and use `kithara_platform`'s re-exports. The
`allowed_crates` list in `.config/arch/thresholds.toml` holds crates whose
production tokio coupling is not yet migrated — those entries are debt to remove,
not a standing exemption.";

#[cfg(test)]
mod tests {
    use toml::Value;

    use super::production_tokio_deps;

    fn parse(src: &str) -> Value {
        toml::from_str(src).expect("toml")
    }

    #[test]
    fn flags_plain_and_target_production_tokio() {
        let m = parse(
            "[dependencies]\n\
             tokio = \"1\"\n\
             [target.'cfg(unix)'.dependencies]\n\
             tokio-util = \"0.7\"\n",
        );
        assert_eq!(production_tokio_deps(&m), vec!["tokio", "tokio-util"]);
    }

    #[test]
    fn ignores_dev_and_build_tokio() {
        let m = parse(
            "[dependencies]\n\
             serde = \"1\"\n\
             [dev-dependencies]\n\
             tokio = { version = \"1\", features = [\"macros\"] }\n\
             [build-dependencies]\n\
             tokio-stream = \"0.1\"\n",
        );
        assert!(
            production_tokio_deps(&m).is_empty(),
            "got: {:?}",
            production_tokio_deps(&m)
        );
    }

    #[test]
    fn ignores_target_dev_dependencies() {
        // `[target.'…'.dev-dependencies]` is not a production table.
        let m = parse(
            "[target.'cfg(not(target_arch = \"wasm32\"))'.dev-dependencies]\n\
             tokio = { version = \"1\", features = [\"rt\"] }\n",
        );
        assert!(
            production_tokio_deps(&m).is_empty(),
            "got: {:?}",
            production_tokio_deps(&m)
        );
    }

    #[test]
    fn no_tokio_is_clean() {
        let m = parse("[dependencies]\nserde = \"1\"\nfutures = \"0.3\"\n");
        assert!(production_tokio_deps(&m).is_empty());
    }

    #[test]
    fn dedupes_tokio_across_tables() {
        let m = parse(
            "[dependencies]\n\
             tokio = \"1\"\n\
             [target.'cfg(unix)'.dependencies]\n\
             tokio = { version = \"1\", features = [\"rt\"] }\n",
        );
        assert_eq!(production_tokio_deps(&m), vec!["tokio"]);
    }
}
