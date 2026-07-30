use std::{fmt, io::Write as _, path::PathBuf};

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::Ctx;

mod adapters;
mod artifact;
mod collect;
mod model;
mod orchestrator;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum AssessmentProfile {
    #[default]
    Product,
    Complete,
}

impl fmt::Display for AssessmentProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Product => "product",
            Self::Complete => "complete",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum AssessmentDepth {
    #[default]
    Standard,
    Deep,
}

impl fmt::Display for AssessmentDepth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Standard => "standard",
            Self::Deep => "deep",
        })
    }
}

#[derive(Debug, Args)]
pub struct AssessArgs {
    /// Include production code only, or every workspace surface.
    #[arg(long, value_enum, default_value_t)]
    pub profile: AssessmentProfile,

    /// Select the standard signal set or every heavyweight analyzer.
    #[arg(long, value_enum, default_value_t)]
    pub depth: AssessmentDepth,

    /// Assess one Cargo package instead of the complete workspace.
    #[arg(long = "crate", conflicts_with = "module")]
    pub krate: Option<String>,

    /// Assess one canonical `package::module` subtree.
    #[arg(long, conflicts_with = "krate")]
    pub module: Option<String>,

    /// Compare against an earlier assessment directory or JSON artifact.
    #[arg(long)]
    pub baseline: Option<PathBuf>,

    /// Rebuild the assessment only from existing artifacts and baselines.
    #[arg(long)]
    pub reuse_existing: bool,
}

pub(crate) fn run(args: &AssessArgs, ctx: &Ctx) -> Result<()> {
    let draft = collect::collect(args, ctx, &[])?;
    let mut stages = if args.reuse_existing {
        orchestrator::reuse(&draft.output_directory)?
    } else {
        orchestrator::refresh(args, ctx, &draft.output_directory)?
    };
    let current = collect::revision(&ctx.root);
    if current.directory != draft.revision {
        stages.push(orchestrator::record_revision_drift(
            &ctx.root,
            &draft.output_directory,
            &draft.revision,
            &current.directory,
        )?);
    }
    let mut assessment = collect::collect(args, ctx, &stages)?;
    assessment.revision = draft.revision;
    assessment.content_digest = draft.content_digest;
    assessment.output_directory = draft.output_directory;
    let artifacts = artifact::write(&assessment, &ctx.root)?;
    writeln!(
        std::io::stdout().lock(),
        "==> {}",
        artifacts.document.display()
    )?;
    if assessment.status == model::AnalysisStatus::Partial {
        bail!("quality assessment is partial; preserved artifacts contain the broken stages");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn assessment_writes_debt_verdict_and_stable_artifacts() {
        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"demo\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        fs::create_dir_all(temp.path().join("demo/src")).expect("source directory");
        fs::write(
            temp.path().join("demo/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("package manifest");
        fs::write(
            temp.path().join("demo/src/lib.rs"),
            "pub struct Demo;\nimpl Demo {\n    pub fn new() -> Self { Self }\n}\n",
        )
        .expect("source");
        for namespace in ["arch", "style", "idioms"] {
            fs::create_dir_all(temp.path().join(".config").join(namespace))
                .expect("baseline directory");
        }
        fs::write(
            temp.path().join(".config/arch/baseline.toml"),
            "schema_version = 1\n\n[file_size]\n\"demo/src/lib.rs\" = 100\n",
        )
        .expect("arch baseline");
        fs::write(
            temp.path().join(".config/style/baseline.toml"),
            "schema_version = 1\n",
        )
        .expect("style baseline");
        fs::write(
            temp.path().join(".config/idioms/baseline.toml"),
            "schema_version = 1\n\n[fat_loop_body]\n\"demo/src/lib.rs\" = 1\n",
        )
        .expect("idioms baseline");
        let ctx = Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("context");
        let args = AssessArgs {
            profile: AssessmentProfile::Product,
            depth: AssessmentDepth::Standard,
            krate: None,
            module: None,
            baseline: None,
            reuse_existing: true,
        };

        run(&args, &ctx).expect("assessment");

        let output = temp
            .path()
            .join("target/quality-assessment/working-tree/product-standard");
        assert!(output.join("manifest.json").is_file());
        assert!(output.join("assessment.md").is_file());
        let assessment: Value = serde_json::from_slice(
            &fs::read(output.join("assessment.json")).expect("assessment JSON"),
        )
        .expect("valid assessment JSON");
        assert_eq!(assessment["summary"]["debt_units"], 101);
        assert_eq!(assessment["summary"]["debt_threshold"], 100);
        assert_eq!(assessment["verdict"], "refactor");
        assert_eq!(assessment["scope"]["kind"], "workspace");
    }

    #[test]
    fn profiles_exclude_tooling_by_default_but_explicit_scope_restores_it() {
        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"demo\", \"tooling\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        for (directory, name) in [("demo", "demo"), ("tooling", "dev-tool")] {
            fs::create_dir_all(temp.path().join(directory).join("src")).expect("source directory");
            fs::write(
                temp.path().join(directory).join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
            )
            .expect("package manifest");
            fs::write(
                temp.path().join(directory).join("src/lib.rs"),
                "pub struct Demo;\n",
            )
            .expect("source");
        }
        fs::create_dir_all(temp.path().join(".config/arch")).expect("baseline directory");
        fs::write(
            temp.path().join(".config/arch/baseline.toml"),
            "schema_version = 1\n\n[file_size]\n\
             \"demo/src/lib.rs\" = 1\n\
             \"tooling/src/lib.rs\" = 100\n",
        )
        .expect("baseline");
        fs::write(
            temp.path().join(".config/xtask.toml"),
            "[architecture.filters]\nexclude_crates = [\"dev-*\"]\n",
        )
        .expect("xtask config");
        let ctx = Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("context");
        let mut args = AssessArgs {
            profile: AssessmentProfile::Product,
            depth: AssessmentDepth::Standard,
            krate: None,
            module: None,
            baseline: None,
            reuse_existing: true,
        };

        run(&args, &ctx).expect("product assessment");
        let product: Value =
            serde_json::from_slice(
                &fs::read(temp.path().join(
                    "target/quality-assessment/working-tree/product-standard/assessment.json",
                ))
                .expect("product assessment"),
            )
            .expect("product JSON");
        assert_eq!(product["summary"]["debt_units"], 1);

        args.profile = AssessmentProfile::Complete;
        run(&args, &ctx).expect("complete assessment");
        let complete: Value =
            serde_json::from_slice(
                &fs::read(temp.path().join(
                    "target/quality-assessment/working-tree/complete-standard/assessment.json",
                ))
                .expect("complete assessment"),
            )
            .expect("complete JSON");
        assert_eq!(complete["summary"]["debt_units"], 101);

        args.profile = AssessmentProfile::Product;
        args.krate = Some("dev-tool".to_owned());
        run(&args, &ctx).expect("explicit tooling assessment");
        let selected: Value =
            serde_json::from_slice(
                &fs::read(temp.path().join(
                    "target/quality-assessment/working-tree/product-standard/assessment.json",
                ))
                .expect("selected assessment"),
            )
            .expect("selected JSON");
        assert_eq!(selected["summary"]["debt_units"], 100);
        assert_eq!(selected["scope"]["name"], "dev-tool");
    }
}
