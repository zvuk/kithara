use std::{collections::BTreeSet, fs, path::Path};

use anyhow::{Context as _, Result};
use toml::Value;

use super::{Check, Context};
use crate::common::{scope::packages_in_scope, violation::Violation};

pub(crate) mod consts {
    pub(crate) const ID: &str = "facade_pool_owner";

    /// The facade that re-exports the pool crate.
    pub(super) const FACADE: &str = "kithara";
    /// The crate that owns the pool types.
    pub(super) const POOL: &str = "kithara-bufpool";

    pub(super) const EXPLANATION: &str = "\
Summary: a crate other than the facade itself names both pool owners: the \
`kithara` facade, which re-exports the pool crate under `kithara::bufpool`, \
and `kithara-bufpool`, which owns the pool types.

Why: the `Pool` derive resolves the owner from the manifest, and a manifest \
that names both owners cannot say which one it means. The facade's `bufpool` \
module is optional, so a facade without that feature — or one that only \
appears in a dev or target-scoped table — leaves the facade path \
unusable while the direct dependency stays valid. The derive refuses such a \
manifest instead of guessing, so the crate does not build.

Fix: keep one owner. A crate that depends on the facade imports the pool types \
from `kithara::bufpool`; a crate that depends on the pool crate imports them \
from `kithara_bufpool`. The facade itself is exempt: it owns the pool crate \
it re-exports.";
}

/// A crate that names one pool owner twice over.
pub(crate) struct FacadePoolOwner;

impl Check for FacadePoolOwner {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.facade_pool_owner;
        let exempt: BTreeSet<&str> = cfg.exempt_crates.iter().map(String::as_str).collect();
        let workspace = workspace_dependencies(ctx.workspace_root)?;
        let mut violations = Vec::new();

        for pkg in packages_in_scope(ctx.metadata, ctx.scope) {
            let name = pkg.name.as_str();
            if exempt.contains(name) {
                continue;
            }
            let manifest = pkg.manifest_path.as_std_path();
            let text = fs::read_to_string(manifest)
                .with_context(|| format!("read manifest: {}", manifest.display()))?;
            let parsed: Value = toml::from_str(&text)
                .with_context(|| format!("parse manifest: {}", manifest.display()))?;

            let owners = named_owners(&parsed, &workspace);
            if owners.contains(consts::FACADE) && owners.contains(consts::POOL) {
                violations.push(
                    Violation::deny(
                        consts::ID,
                        format!("{name}::{}+{}", consts::FACADE, consts::POOL),
                        format!(
                            "`{name}` depends on both the `{}` facade and `{}`; \
                             a pool schema can name only one owner",
                            consts::FACADE,
                            consts::POOL
                        ),
                    )
                    .with_explanation(consts::EXPLANATION.into()),
                );
            }
        }
        Ok(violations)
    }
}

/// The workspace's shared dependency table, which inherited entries point at.
fn workspace_dependencies(workspace_root: &Path) -> Result<Value> {
    let manifest = workspace_root.join("Cargo.toml");
    let text = fs::read_to_string(&manifest)
        .with_context(|| format!("read manifest: {}", manifest.display()))?;
    let parsed: Value =
        toml::from_str(&text).with_context(|| format!("parse manifest: {}", manifest.display()))?;
    Ok(parsed
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .cloned()
        .unwrap_or_else(|| Value::Table(toml::map::Map::new())))
}

/// The packages a manifest names, in every scope the resolver reads.
///
/// The scopes are the ones a proc macro can see: `dependencies`,
/// `dev-dependencies`, and the same two tables under a target. Build
/// dependencies are not among them.
fn named_owners(manifest: &Value, workspace: &Value) -> BTreeSet<String> {
    let mut owners = BTreeSet::new();
    collect(manifest.get("dependencies"), workspace, &mut owners);
    collect(manifest.get("dev-dependencies"), workspace, &mut owners);
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for spec in targets.values() {
            collect(spec.get("dependencies"), workspace, &mut owners);
            collect(spec.get("dev-dependencies"), workspace, &mut owners);
        }
    }
    owners
}

fn collect(table: Option<&Value>, workspace: &Value, out: &mut BTreeSet<String>) {
    let Some(table) = table.and_then(Value::as_table) else {
        return;
    };
    for (key, value) in table {
        out.insert(named_package(key, value, workspace));
    }
}

/// The package a dependency entry names.
///
/// A renamed dependency names its package through `package =`; an inherited
/// one names it through the workspace's shared table.
fn named_package(key: &str, value: &Value, workspace: &Value) -> String {
    let Some(spec) = value.as_table() else {
        return key.to_owned();
    };
    if let Some(package) = spec.get("package").and_then(Value::as_str) {
        return package.to_owned();
    }
    if spec.get("workspace").and_then(Value::as_bool) == Some(true) {
        let inherited = workspace.get(key).and_then(Value::as_table);
        if let Some(package) = inherited
            .and_then(|entry| entry.get("package"))
            .and_then(Value::as_str)
        {
            return package.to_owned();
        }
    }
    key.to_owned()
}

#[cfg(test)]
mod tests {
    use toml::Value;

    use super::{named_owners, named_package};

    fn parse(src: &str) -> Value {
        toml::from_str(src).expect("toml")
    }

    fn owners(src: &str, workspace: &str) -> Vec<String> {
        named_owners(&parse(src), &workspace_table(workspace))
            .into_iter()
            .collect()
    }

    /// The workspace's shared dependency table, as the check reads it.
    fn workspace_table(src: &str) -> Value {
        parse(src)
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies"))
            .cloned()
            .unwrap_or_else(|| Value::Table(toml::map::Map::new()))
    }

    #[test]
    fn flags_a_literal_dual_owner_manifest() {
        let found = owners(
            "[dependencies]\n\
             kithara = { path = \"../kithara\" }\n\
             kithara-bufpool = { path = \"../kithara-bufpool\" }\n",
            "",
        );
        assert_eq!(found, vec!["kithara", "kithara-bufpool"]);
    }

    #[test]
    fn flags_a_renamed_dual_owner_manifest() {
        let found = owners(
            "[dependencies]\n\
             engine = { package = \"kithara\", version = \"0.1\" }\n\
             pool = { package = \"kithara-bufpool\", version = \"0.1\" }\n",
            "",
        );
        assert_eq!(found, vec!["kithara", "kithara-bufpool"]);
    }

    #[test]
    fn flags_a_workspace_inherited_dual_owner_manifest() {
        let found = owners(
            "[dependencies]\n\
             kithara = { workspace = true }\n\
             kithara-bufpool = { workspace = true }\n",
            "[workspace.dependencies]\n\
             kithara = { path = \"../kithara\" }\n\
             kithara-bufpool = { path = \"../kithara-bufpool\" }\n",
        );
        assert_eq!(found, vec!["kithara", "kithara-bufpool"]);
    }

    #[test]
    fn flags_a_workspace_inherited_rename() {
        let found = owners(
            "[dependencies]\n\
             engine = { workspace = true }\n\
             pool = { workspace = true }\n",
            "[workspace.dependencies]\n\
             engine = { package = \"kithara\", path = \"../kithara\" }\n\
             pool = { package = \"kithara-bufpool\", path = \"../kithara-bufpool\" }\n",
        );
        assert_eq!(found, vec!["kithara", "kithara-bufpool"]);
    }

    #[test]
    fn reads_the_dev_and_target_scopes_the_resolver_reads() {
        let found = owners(
            "[dependencies]\n\
             kithara-bufpool = { path = \"../kithara-bufpool\" }\n\n\
             [dev-dependencies]\n\
             kithara = { path = \"../kithara\" }\n\n\
             [target.'cfg(windows)'.dependencies]\n\
             kithara = { path = \"../kithara\" }\n",
            "",
        );
        assert_eq!(found, vec!["kithara", "kithara-bufpool"]);
    }

    #[test]
    fn leaves_a_direct_owner_only_manifest_alone() {
        let found = owners(
            "[dependencies]\n\
             kithara-bufpool = { path = \"../kithara-bufpool\" }\n",
            "",
        );
        assert_eq!(found, vec!["kithara-bufpool"]);
    }

    #[test]
    fn leaves_a_facade_only_manifest_alone() {
        let found = owners("[dependencies]\nkithara = { path = \"../kithara\" }\n", "");
        assert_eq!(found, vec!["kithara"]);
    }

    #[test]
    fn ignores_build_dependencies_the_resolver_never_reads() {
        let found = owners(
            "[build-dependencies]\n\
             kithara = { path = \"../kithara\" }\n",
            "",
        );
        assert!(found.is_empty(), "got: {found:?}");
    }

    #[test]
    fn a_version_only_dependency_names_its_own_key() {
        let package = named_package(
            "kithara",
            &Value::String("1".to_owned()),
            &Value::Table(toml::map::Map::new()),
        );
        assert_eq!(package, "kithara");
    }
}
