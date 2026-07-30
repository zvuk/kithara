use std::path::PathBuf;

use clap::{Args, ValueEnum};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ViewName {
    #[default]
    Overview,
    Hierarchy,
    Ownership,
}

impl ViewName {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Hierarchy => "hierarchy",
            Self::Ownership => "ownership",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum SemanticMode {
    #[default]
    Auto,
    Off,
    Required,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum RuntimeMode {
    #[default]
    Auto,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum Lod {
    #[default]
    Auto,
    #[value(name = "0")]
    Level0,
    #[value(name = "1")]
    Level1,
    #[value(name = "2")]
    Level2,
    #[value(name = "3")]
    Level3,
    #[value(name = "4")]
    Level4,
}

impl Lod {
    pub(crate) fn resolve(self, automatic: u8) -> u8 {
        match self {
            Self::Auto => automatic,
            Self::Level0 => 0,
            Self::Level1 => 1,
            Self::Level2 => 2,
            Self::Level3 => 3,
            Self::Level4 => 4,
        }
    }
}

#[derive(Debug, Args)]
pub struct VizArgs {
    /// Projection rendered from the shared evidence graph.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) view: ViewName,

    /// Architecture detail from crates (0) to the full evidence graph (4).
    #[arg(long, value_enum, default_value_t)]
    pub(crate) lod: Lod,

    /// Focus on one Cargo package and its outgoing public boundary.
    #[arg(long = "crate")]
    pub(crate) krate: Option<String>,

    /// Restrict the projection to one module path and its descendants.
    #[arg(long)]
    pub(crate) module: Option<String>,

    /// Exclude Cargo packages from semantic selection and the visible projection.
    #[arg(long = "exclude-crate")]
    pub(crate) exclude_crates: Vec<String>,

    /// Exclude canonical `package::module` subtrees from the visible projection.
    #[arg(long = "exclude-module")]
    pub(crate) exclude_modules: Vec<String>,

    /// Ignore project-default exclusions while preserving explicit filters.
    #[arg(long)]
    pub(crate) include_default_excluded: bool,

    /// Semantic call resolution policy.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) semantic: SemanticMode,

    /// Run only one configured runtime scenario.
    #[arg(long)]
    pub(crate) scenario: Option<String>,

    /// Merge a previously captured manual JSONL trace.
    #[arg(long)]
    pub(crate) trace: Option<PathBuf>,

    /// Configured runtime scenario policy.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) runtime: RuntimeMode,
}

#[cfg(test)]
mod tests {
    use clap::{Parser, Subcommand};

    use super::{Lod, RuntimeMode, SemanticMode, ViewName, VizArgs};

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Debug, Subcommand)]
    enum Command {
        Viz(VizArgs),
    }

    #[test]
    fn viz_does_not_require_a_nested_subcommand() {
        let cli = Cli::try_parse_from(["xtask", "viz"]).expect("viz should parse without a view");
        let Command::Viz(args) = cli.command;
        assert_eq!(args.view, ViewName::Overview);
        assert_eq!(args.lod, Lod::Auto);
        assert_eq!(args.semantic, SemanticMode::Auto);
        assert_eq!(args.runtime, RuntimeMode::Auto);
    }

    #[test]
    fn viz_accepts_numeric_detail_levels() {
        let cli =
            Cli::try_parse_from(["xtask", "viz", "--lod", "0"]).expect("numeric LOD should parse");
        let Command::Viz(args) = cli.command;
        assert_eq!(args.lod, Lod::Level0);
    }

    #[test]
    fn viz_keeps_filters_on_one_command() {
        let cli = Cli::try_parse_from([
            "xtask",
            "viz",
            "--view",
            "ownership",
            "--crate",
            "demo",
            "--module",
            "runtime",
        ])
        .expect("viz filters should parse");
        let Command::Viz(args) = cli.command;
        assert_eq!(args.view, ViewName::Ownership);
        assert_eq!(args.krate.as_deref(), Some("demo"));
        assert_eq!(args.module.as_deref(), Some("runtime"));
    }

    #[test]
    fn viz_accepts_repeated_exclusion_filters() {
        let cli = Cli::try_parse_from([
            "xtask",
            "viz",
            "--exclude-crate",
            "test-*",
            "--exclude-crate",
            "xtask",
            "--exclude-module",
            "*::tests",
            "--exclude-module",
            "*::fixtures",
        ])
        .expect("viz exclusion filters should parse");
        let Command::Viz(args) = cli.command;
        assert_eq!(args.exclude_crates, ["test-*", "xtask"]);
        assert_eq!(args.exclude_modules, ["*::tests", "*::fixtures"]);
    }

    #[test]
    fn viz_can_include_scopes_excluded_by_project_defaults() {
        let cli = Cli::try_parse_from(["xtask", "viz", "--include-default-excluded"]);

        assert!(
            cli.is_ok(),
            "complete profile override should parse: {cli:?}"
        );
    }
}
