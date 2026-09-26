use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use kithara_devtools::Ctx;
use serde::Deserialize;
use tracing::info;

use crate::consts;

#[derive(Debug, Args)]
pub(crate) struct MutantsArgs {
    #[command(subcommand)]
    command: MutantsCommand,
}

#[derive(Debug, Subcommand)]
enum MutantsCommand {
    /// List the explicitly allowed mutation suites.
    List,
    /// Run one explicitly allowed mutation suite.
    Run {
        /// Suite name from .config/mutation-suites.toml.
        suite: String,
        /// Output directory for cargo-mutants reports.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Concurrent cargo-mutants jobs.
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// Run only this slice of the suite's mutants, written `k/n`,
        /// numbered from zero.
        #[arg(long)]
        shard: Option<Shard>,
    },
    /// Run every explicitly allowed mutation suite sequentially.
    RunAll {
        /// Parent output directory for cargo-mutants reports.
        #[arg(long, default_value = "target/mutants-ci")]
        output: PathBuf,
        /// Concurrent cargo-mutants jobs within one suite.
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// Run only the suites in this group; omit to run every suite.
        ///
        /// A group is one CI lane. Ten suites in one lane are more than its
        /// window: the run reached the ninth at 100 minutes and was cut with
        /// the last two unread. Groups also keep a lane's builds down to the
        /// packages its own suites name, and leave a green group alone while
        /// another is still red.
        #[arg(long)]
        group: Option<String>,
        /// Run only this slice of each suite's mutants, written `k/n`,
        /// numbered from zero.
        ///
        /// A suite over one file cannot be split between groups; this splits
        /// its mutants instead, so a lane reports a verdict inside its window.
        #[arg(long)]
        shard: Option<Shard>,
    },
}

/// One slice of a suite's mutants, written `k/n` on the command line.
///
/// `cargo-mutants` numbers shards from zero, so `k` runs from `0` to `n - 1`
/// and `n/n` names nothing. A lane that asked for `4/4` was refused outright,
/// which also means the `0/4` nobody asked for would have gone untested while
/// every lane reported success.
///
/// A group divides suites between lanes, but a suite over a single file cannot
/// be divided that way: the UI size rules are 128 mutants in one file, and each
/// rebuilds a heavy crate. Two runs in a row spent the lane's whole window and
/// were cut without reporting a verdict. A shard divides the mutants of the
/// suites a lane already holds, so the lane finishes inside its window without
/// the window being widened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[display("{index}/{total}")]
struct Shard {
    index: usize,
    total: usize,
}

impl FromStr for Shard {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let (index, total) = text
            .split_once('/')
            .with_context(|| format!("a shard reads `i/n`, got `{text}`"))?;
        let index: usize = index
            .parse()
            .with_context(|| format!("shard index in `{text}`"))?;
        let total: usize = total
            .parse()
            .with_context(|| format!("shard count in `{text}`"))?;
        if total == 0 || index >= total {
            bail!("a shard is `k/n` with 0 <= k < n, got `{text}`");
        }
        Ok(Self { index, total })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationConfig {
    suite: Vec<MutationSuite>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationSuite {
    name: String,
    /// CI lane this suite runs in. See `.config/mutation-suites.toml`.
    group: String,
    package: String,
    files: Vec<PathBuf>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    exclude_re: Vec<String>,
    test_filters: Vec<String>,
    timeout_seconds: u64,
}

pub(crate) fn run(args: &MutantsArgs, ctx: &Ctx) -> Result<()> {
    let config = MutationConfig::load(&ctx.root)?;
    match &args.command {
        MutantsCommand::List => {
            for suite in &config.suite {
                info!(
                    suite = suite.name,
                    package = suite.package,
                    files = suite
                        .files
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    tests = suite.test_filters.join(","),
                    "focused mutation suite"
                );
            }
            Ok(())
        }
        MutantsCommand::Run {
            suite,
            output,
            jobs,
            shard,
        } => {
            let suite = config.named(suite)?;
            let output = output.as_deref().map_or_else(
                || ctx.root.join("target/mutants").join(&suite.name),
                |path| rooted(&ctx.root, path),
            );
            suite.execute(&ctx.root, &output, *jobs, *shard)
        }
        MutantsCommand::RunAll {
            output,
            jobs,
            group,
            shard,
        } => {
            let output = rooted(&ctx.root, output);
            let selected = config.in_group(group.as_deref())?;
            for suite in selected {
                suite.execute(&ctx.root, &output.join(&suite.name), *jobs, *shard)?;
            }
            Ok(())
        }
    }
}

fn rooted(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

impl MutationConfig {
    fn load(root: &Path) -> Result<Self> {
        let path = root.join(consts::CONFIG_PATH);
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
        config.validate(root)?;
        Ok(config)
    }

    /// The suites in `group`, or every suite when no group is named.
    ///
    /// A group nobody declares is a typo in a lane, not an empty run: the
    /// lane would report success having tested nothing.
    fn in_group(&self, group: Option<&str>) -> Result<Vec<&MutationSuite>> {
        let Some(group) = group else {
            return Ok(self.suite.iter().collect());
        };
        let selected: Vec<&MutationSuite> = self
            .suite
            .iter()
            .filter(|suite| suite.group == group)
            .collect();
        if selected.is_empty() {
            let known: BTreeSet<&str> = self.suite.iter().map(|s| s.group.as_str()).collect();
            bail!(
                "no mutation suite is in group `{group}`; {CONFIG_PATH} declares {}",
                known.into_iter().collect::<Vec<_>>().join(", "),
                CONFIG_PATH = consts::CONFIG_PATH
            );
        }
        Ok(selected)
    }

    fn validate(&self, root: &Path) -> Result<()> {
        if self.suite.is_empty() {
            bail!(
                "{CONFIG_PATH} must define at least one [[suite]]",
                CONFIG_PATH = consts::CONFIG_PATH
            );
        }

        let mut names = BTreeSet::new();
        for suite in &self.suite {
            suite.validate(root)?;
            if !names.insert(&suite.name) {
                bail!("duplicate mutation suite: {}", suite.name);
            }
        }
        Ok(())
    }

    fn named(&self, name: &str) -> Result<&MutationSuite> {
        self.suite
            .iter()
            .find(|suite| suite.name == name)
            .with_context(|| format!("unknown mutation suite: {name}"))
    }
}

impl MutationSuite {
    fn validate(&self, root: &Path) -> Result<()> {
        if self.name.is_empty()
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            bail!(
                "mutation suite names must use lowercase ASCII, digits, and dashes: {}",
                self.name
            );
        }
        if self.package.trim().is_empty() {
            bail!("mutation suite {} has no package", self.name);
        }
        if self.group.trim().is_empty() {
            bail!(
                "mutation suite {} names no group, so no lane runs it",
                self.name
            );
        }
        if self.files.is_empty() {
            bail!("mutation suite {} has no production files", self.name);
        }
        for file in &self.files {
            if file.is_absolute()
                || file.extension().and_then(|ext| ext.to_str()) != Some("rs")
                || file
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
                || !root.join(file).is_file()
            {
                bail!(
                    "mutation suite {} has invalid production file: {}",
                    self.name,
                    file.display()
                );
            }
            if !file.starts_with("crates")
                || !file.components().any(|part| part.as_os_str() == "src")
            {
                bail!(
                    "mutation suite {} may mutate only crate src files: {}",
                    self.name,
                    file.display()
                );
            }
        }
        if self.test_filters.is_empty()
            || self
                .test_filters
                .iter()
                .any(|filter| !filter.starts_with("test(") || !filter.ends_with(')'))
        {
            bail!(
                "mutation suite {} must define explicit nextest test() filters",
                self.name
            );
        }
        if self.exclude_re.iter().any(String::is_empty) {
            bail!(
                "mutation suite {} must not define an empty exclude_re pattern",
                self.name
            );
        }
        if !(10..=900).contains(&self.timeout_seconds) {
            bail!(
                "mutation suite {} timeout must be between 10 and 900 seconds",
                self.name
            );
        }
        Ok(())
    }

    fn execute(&self, root: &Path, output: &Path, jobs: usize, shard: Option<Shard>) -> Result<()> {
        if jobs == 0 {
            bail!("mutation jobs must be positive");
        }
        fs::create_dir_all(output)
            .with_context(|| format!("creating mutation output {}", output.display()))?;
        info!(
            suite = self.name,
            shard = shard.map(|s| s.to_string()),
            "starting mutation suite"
        );
        let status = self
            .command(root, output, jobs, shard)
            .status()
            .with_context(|| format!("running mutation suite {}", self.name))?;
        if !status.success() {
            bail!("mutation suite {} failed with {status}", self.name);
        }
        Ok(())
    }

    fn command(&self, root: &Path, output: &Path, jobs: usize, shard: Option<Shard>) -> Command {
        let mut command = Command::new("cargo");
        // cargo-mutants gives every mutant its own copied source tree. A CI
        // runner's shared target directory would make concurrent mutant builds
        // overwrite artifacts for the same package/version, so leave target
        // selection to cargo-mutants' isolated tree.
        command.env_remove("CARGO_TARGET_DIR");
        command
            .current_dir(root)
            .arg("mutants")
            .arg("--package")
            .arg(&self.package)
            .arg("--baseline=run")
            .arg("--test-tool=nextest")
            .arg("--profile=test-release")
            .arg("--no-shuffle")
            .arg("--jobs")
            .arg(jobs.to_string())
            .arg("--timeout")
            .arg(self.timeout_seconds.to_string())
            .arg("--minimum-test-timeout")
            .arg(self.timeout_seconds.to_string())
            .arg("--output")
            .arg(output)
            .arg("--cargo-test-arg=--lib")
            // A suite names its own filterset over one file. The test profile's
            // `default-filter` exists to keep `just test` off the lanes that own
            // their own runner, and it silently subtracts from that filterset:
            // for a package it excludes outright, the suite selects nothing and
            // the run dies on an empty baseline rather than on a mutant.
            .arg("--cargo-test-arg=--ignore-default-filter")
            .arg("--cargo-test-arg=-E")
            .arg(format!(
                "--cargo-test-arg={}",
                combined_test_filter(&self.test_filters)
            ));

        for file in &self.files {
            command.arg("--file").arg(file);
        }
        for pattern in &self.exclude_re {
            command.arg("--exclude-re").arg(pattern);
        }
        if !self.features.is_empty() {
            command.arg("--features").arg(self.features.join(","));
        }
        if let Some(shard) = shard {
            command.arg("--shard").arg(shard.to_string());
        }
        command
    }
}

fn combined_test_filter(filters: &[String]) -> String {
    if filters.len() == 1 {
        filters[0].clone()
    } else {
        format!("any({})", filters.join(","))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn write_fixture(root: &Path, content: &str) {
        let source = root.join("crates/example/src/lib.rs");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(source, "pub fn value() -> bool { true }\n").unwrap();
        fs::create_dir_all(root.join(".config")).unwrap();
        fs::write(root.join(consts::CONFIG_PATH), content).unwrap();
    }

    /// The suites the repository ships, not a fixture. Every other test here
    /// proves the machinery against a synthetic config and never opens the file
    /// the `deep-mutants` lanes run, which is how `crossfader` came to name a
    /// production file that had moved. Validation covers the whole config
    /// before any suite runs, so one stale path failed `list`, `run` and
    /// `run-all` alike: the lane spent its week reporting an error instead of
    /// a surviving mutant, and nothing on the push gate could say so.
    #[test]
    fn the_shipped_suites_still_name_the_files_they_mutate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask sits beside the workspace root");
        MutationConfig::load(root).unwrap_or_else(|error| {
            panic!(
                "the shipped {CONFIG_PATH} is unusable: {error:#}",
                CONFIG_PATH = consts::CONFIG_PATH
            )
        });
    }

    #[test]
    fn suite_command_is_package_file_and_unit_test_scoped() {
        let root = tempdir().unwrap();
        write_fixture(
            root.path(),
            r#"
[[suite]]
name = "small"
group = "stream"
package = "example"
files = ["crates/example/src/lib.rs"]
exclude_re = ["example::debug"]
test_filters = ["test(/tests::value/)"]
timeout_seconds = 30
"#,
        );
        let config = MutationConfig::load(root.path()).unwrap();
        let suite = config.named("small").unwrap();
        let command = suite.command(root.path(), Path::new("out"), 2, None);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.windows(2).any(|args| args == ["--package", "example"]));
        assert!(
            args.windows(2)
                .any(|args| args == ["--file", "crates/example/src/lib.rs"])
        );
        assert!(
            args.windows(2)
                .any(|args| args == ["--exclude-re", "example::debug"])
        );
        assert!(args.contains(&"--cargo-test-arg=--lib".to_string()));
        assert!(args.contains(&"--cargo-test-arg=--ignore-default-filter".to_string()));
        assert!(args.contains(&"--cargo-test-arg=test(/tests::value/)".to_string()));
        assert!(!args.contains(&"--workspace".to_string()));
        assert!(!args.iter().any(|arg| arg.starts_with("--test-workspace")));
        assert!(
            command
                .get_envs()
                .any(|(name, value)| { name == "CARGO_TARGET_DIR" && value.is_none() })
        );
    }

    #[test]
    fn rejects_unscoped_or_escaping_files() {
        let root = tempdir().unwrap();
        write_fixture(
            root.path(),
            r#"
[[suite]]
name = "small"
group = "stream"
package = "example"
files = ["../outside.rs"]
test_filters = ["test(/tests::value/)"]
timeout_seconds = 30
"#,
        );

        assert!(MutationConfig::load(root.path()).is_err());
    }

    #[test]
    fn a_group_selects_only_its_own_suites() {
        let root = tempdir().unwrap();
        write_fixture(
            root.path(),
            r#"
[[suite]]
name = "first"
group = "stream"
package = "example"
files = ["crates/example/src/lib.rs"]
test_filters = ["test(/tests::value/)"]
timeout_seconds = 30

[[suite]]
name = "second"
group = "ui"
package = "example"
files = ["crates/example/src/lib.rs"]
test_filters = ["test(/tests::value/)"]
timeout_seconds = 30
"#,
        );
        let config = MutationConfig::load(root.path()).expect("a valid fixture");

        let selected = config.in_group(Some("stream")).expect("a declared group");

        assert_eq!(
            selected.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["first"]
        );
    }

    #[test]
    fn no_group_runs_every_suite() {
        let root = tempdir().unwrap();
        write_fixture(
            root.path(),
            r#"
[[suite]]
name = "first"
group = "stream"
package = "example"
files = ["crates/example/src/lib.rs"]
test_filters = ["test(/tests::value/)"]
timeout_seconds = 30
"#,
        );
        let config = MutationConfig::load(root.path()).expect("a valid fixture");

        assert_eq!(config.in_group(None).expect("every suite").len(), 1);
    }

    #[test]
    fn a_group_nobody_declares_is_refused() {
        let root = tempdir().unwrap();
        write_fixture(
            root.path(),
            r#"
[[suite]]
name = "first"
group = "stream"
package = "example"
files = ["crates/example/src/lib.rs"]
test_filters = ["test(/tests::value/)"]
timeout_seconds = 30
"#,
        );
        let config = MutationConfig::load(root.path()).expect("a valid fixture");

        assert!(config.in_group(Some("no-such-group")).is_err());
    }

    /// A lane addresses its slice on the command line, so the flag has to reach
    /// cargo-mutants verbatim; a shard the tool never sees would run the whole
    /// suite twice and report it as two verdicts.
    #[test]
    fn a_shard_reaches_the_mutation_tool() {
        let root = tempdir().unwrap();
        write_fixture(
            root.path(),
            r#"
[[suite]]
name = "small"
group = "ui"
package = "example"
files = ["crates/example/src/lib.rs"]
test_filters = ["test(/tests::value/)"]
timeout_seconds = 30
"#,
        );
        let config = MutationConfig::load(root.path()).unwrap();
        let suite = config.named("small").unwrap();
        let shard = Shard::from_str("2/4").unwrap();
        let command = suite.command(root.path(), Path::new("out"), 2, Some(shard));
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.windows(2).any(|args| args == ["--shard", "2/4"]));
    }

    /// Shards are numbered from zero, the way the tool numbers them, so `4/4`
    /// names nothing. A lane that asked for it was refused by cargo-mutants
    /// itself -- and had the count been read the other way, the `0/4` nobody
    /// asked for would have gone untested while every lane reported success.
    #[test]
    fn a_shard_outside_its_own_count_is_refused() {
        assert!(Shard::from_str("4/4").is_err());
        assert!(Shard::from_str("5/4").is_err());
        assert!(Shard::from_str("1/0").is_err());
        assert!(Shard::from_str("half").is_err());
        assert_eq!(Shard::from_str("0/4").unwrap().to_string(), "0/4");
        assert_eq!(Shard::from_str("3/4").unwrap().to_string(), "3/4");
    }
}
