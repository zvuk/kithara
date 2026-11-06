use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use reqwest::Url;
use serde::Deserialize;
use tracing::info;

use super::{
    ledger::Ledger,
    model::{regular_file, simple_branch, simple_repository, validate_sha},
    reconcile::Bridge,
};

#[derive(Debug, Args)]
pub(crate) struct BridgeArgs {
    #[command(subcommand)]
    command: BridgeCommand,
}

#[derive(Debug, Subcommand)]
enum BridgeCommand {
    /// Validate configuration and secret file references without network access.
    Validate {
        /// Bridge configuration file.
        #[arg(long, env = "KITHARA_BRIDGE_CONFIG")]
        config: PathBuf,
    },
    /// Reconcile both repository heads once and exit.
    Reconcile {
        /// Bridge configuration file.
        #[arg(long, env = "KITHARA_BRIDGE_CONFIG")]
        config: PathBuf,
    },
    /// Retry one terminal verification identified by its exact head and base.
    Retry {
        /// Bridge configuration file.
        #[arg(long, env = "KITHARA_BRIDGE_CONFIG")]
        config: PathBuf,
        /// Exact GitHub pull-request head SHA.
        #[arg(long)]
        github_sha: String,
        /// Exact synchronized base SHA used by the verification.
        #[arg(long)]
        base_sha: String,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BridgeConfig {
    pub(super) github_repo: String,
    pub(super) github_token_file: PathBuf,
    pub(super) gitlab_url: Url,
    pub(super) gitlab_project_id: u64,
    pub(super) gitlab_project_path: String,
    pub(super) gitlab_username: String,
    pub(super) gitlab_token_file: PathBuf,
    /// The default branch on each side, named separately.
    ///
    /// One key served both while the two repositories agreed on the name. They
    /// no longer do: GitLab calls its default branch `develop` and GitHub calls
    /// its own `main`, and a single value cannot be right for both.
    pub(super) github_branch: String,
    pub(super) gitlab_branch: String,
    pub(super) state_dir: PathBuf,
    /// GitHub logins whose pull requests may change the CI control paths
    /// directly.
    ///
    /// Declared here, in the host's own configuration, and never in the
    /// repository: a list a pull request could edit would be a list that adds
    /// its own author. Empty by default, which is the behaviour every other
    /// contributor gets.
    #[serde(default)]
    pub(super) trusted_authors: Vec<String>,
}

impl BridgeConfig {
    fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading bridge config {}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("parsing bridge config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if !simple_repository(&self.github_repo) {
            bail!("github_repo must use a safe owner/name form");
        }
        if !simple_repository(&self.gitlab_project_path) {
            bail!("gitlab_project_path must use a safe group/name form");
        }
        if self.gitlab_project_id == 0 {
            bail!("GitLab project id must be positive");
        }
        for (label, path) in [
            ("GitHub token", &self.github_token_file),
            ("GitLab token", &self.gitlab_token_file),
        ] {
            if !regular_file(path) {
                bail!("{label} not found: {}", path.display());
            }
        }
        if self.gitlab_url.scheme() != "https"
            || self.gitlab_url.host_str().is_none()
            || !self.gitlab_url.username().is_empty()
            || self.gitlab_url.password().is_some()
            || self.gitlab_url.query().is_some()
            || self.gitlab_url.fragment().is_some()
        {
            bail!("gitlab_url must be an HTTPS origin without credentials or query");
        }
        for (label, branch) in [
            ("github_branch", &self.github_branch),
            ("gitlab_branch", &self.gitlab_branch),
        ] {
            if !simple_branch(branch) {
                bail!("{label} must be one simple branch name");
            }
        }
        if self.gitlab_username.is_empty()
            || !self
                .gitlab_username
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            bail!("gitlab_username contains unsupported characters");
        }
        if !self.state_dir.is_absolute()
            || self.state_dir.parent().is_none()
            || self.state_dir.parent() == Some(Path::new("/"))
        {
            bail!("state_dir must be an absolute path below a dedicated directory");
        }
        Ok(())
    }

    pub(super) fn gitlab_origin(&self) -> String {
        self.gitlab_url.as_str().trim_end_matches('/').to_string()
    }

    pub(super) fn gitlab_pipeline_url(&self, pipeline_id: u64) -> String {
        format!(
            "{}/{}/-/pipelines/{pipeline_id}",
            self.gitlab_origin(),
            self.gitlab_project_path
        )
    }
}

/// The secrets a bridge configuration points at, for the installer that has to
/// check who owns them before activating the service.
///
/// It reads them from here rather than declaring the file's shape a second time:
/// the copy that did went on asking for the GitHub App private key after the
/// bridge moved to a token, so activation would have refused every correct
/// configuration — and only at the last step of the switchover.
pub(crate) fn secret_files(config: &Path) -> Result<[PathBuf; 2]> {
    let config = BridgeConfig::load(config)?;
    Ok([config.github_token_file, config.gitlab_token_file])
}

pub(crate) fn run(args: &BridgeArgs) -> Result<()> {
    match &args.command {
        BridgeCommand::Validate { config } => {
            BridgeConfig::load(config)?;
            Ok(())
        }
        BridgeCommand::Reconcile { config } => {
            Bridge::new(BridgeConfig::load(config)?)?.reconcile_once()
        }
        BridgeCommand::Retry {
            config,
            github_sha,
            base_sha,
        } => {
            if !validate_sha(github_sha) || !validate_sha(base_sha) {
                bail!("github-sha and base-sha must be full 40-character commit SHAs");
            }
            let config = BridgeConfig::load(config)?;
            let entry = Ledger::new(&config.state_dir)?.retry(github_sha, base_sha)?;
            info!(
                %github_sha,
                %base_sha,
                attempt = entry.attempt,
                pipeline_id = ?entry.pipeline_id,
                "terminal verification reserved for exact-key retry"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::BridgeConfig;
    use crate::Cli;

    fn example() -> String {
        let path = crate::ci::config::workspace_root().join(".config/bridge/config.example.toml");
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn pipeline_url_has_one_slash_after_a_trailing_slash_origin() {
        let mut config = toml::from_str::<BridgeConfig>(&example()).unwrap();
        config.gitlab_url = "https://gitlab.example/".parse().unwrap();
        config.gitlab_project_path = "team/audio".into();

        assert_eq!(
            config.gitlab_pipeline_url(42),
            "https://gitlab.example/team/audio/-/pipelines/42"
        );
    }

    /// The list lives in the host's configuration and nowhere else. A pull
    /// request cannot reach it, which is the whole point: a repository-side
    /// list would be one a pull request can add its own author to. Absent by
    /// default, so every contributor gets the rule until the host says
    /// otherwise.
    #[test]
    fn trusted_authors_come_from_the_host_configuration_and_default_to_none() {
        let none = toml::from_str::<BridgeConfig>(&example()).unwrap();
        assert!(none.trusted_authors.is_empty());

        let listed = toml::from_str::<BridgeConfig>(&format!(
            "{}trusted_authors = [\"gerasim13\"]\n",
            example()
        ))
        .unwrap();
        assert_eq!(listed.trusted_authors, ["gerasim13"]);
    }

    #[test]
    fn bridge_config_trusts_the_platform_store_only() {
        toml::from_str::<BridgeConfig>(&example()).unwrap();
        let with_ca = format!("{}gitlab_ca_file = \"/path/to/ca.crt\"\n", example());
        assert!(
            toml::from_str::<BridgeConfig>(&with_ca).is_err(),
            "bridge config must reject a private CA instead of silently ignoring it"
        );
    }

    /// What the installer checks ownership of before it activates the service.
    /// It used to read the file through a declaration of its own, which went on
    /// asking for a GitHub App private key long after the bridge moved to a
    /// token — so both tokens have to come back from the configuration the
    /// bridge itself parses, and both have to be there.
    #[test]
    fn the_installer_is_handed_both_tokens_from_the_bridge_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let github = directory.path().join("github.token");
        let gitlab = directory.path().join("gitlab.token");
        std::fs::write(&github, "token\n").unwrap();
        std::fs::write(&gitlab, "token\n").unwrap();
        let config = directory.path().join("config.toml");
        std::fs::write(
            &config,
            example()
                .replace("/path/to/github-token", github.to_str().unwrap())
                .replace("/path/to/gitlab-token", gitlab.to_str().unwrap())
                .replace(
                    "/path/to/bridge-state",
                    directory.path().join("state").to_str().unwrap(),
                ),
        )
        .unwrap();

        assert_eq!(super::secret_files(&config).unwrap(), [github, gitlab]);
    }

    #[test]
    fn bridge_requires_an_explicit_operation() {
        assert!(Cli::try_parse_from(["xtask", "ci", "bridge"]).is_err());
        assert!(
            Cli::try_parse_from([
                "xtask",
                "ci",
                "bridge",
                "validate",
                "--config",
                "bridge.toml"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "xtask",
                "ci",
                "bridge",
                "reconcile",
                "--config",
                "bridge.toml"
            ])
            .is_ok()
        );
    }

    #[test]
    fn retry_requires_both_exact_key_components() {
        assert!(
            Cli::try_parse_from([
                "xtask",
                "ci",
                "bridge",
                "retry",
                "--config",
                "bridge.toml",
                "--github-sha",
                "0123456789abcdef0123456789abcdef01234567",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "xtask",
                "ci",
                "bridge",
                "retry",
                "--config",
                "bridge.toml",
                "--github-sha",
                "0123456789abcdef0123456789abcdef01234567",
                "--base-sha",
                "89abcdef0123456789abcdef0123456789abcdef",
            ])
            .is_ok()
        );
    }
}
