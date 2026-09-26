use std::{
    env,
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::consts;

/// A repository the release reaches over HTTPS. The token travels in an HTTP
/// header set through the environment, so it lands in no URL, remote
/// configuration, or argument list.
pub(super) struct Remote {
    pub(super) name: &'static str,
    pub(super) url: String,
    header: Option<String>,
}

impl Remote {
    /// GitHub reads a public repository without a token; pushing needs one.
    pub(super) fn github(repo: &str, token: Option<&str>) -> Result<Self> {
        Ok(Self {
            name: "github",
            url: github_remote(repo)?,
            header: token.map(|token| basic("x-access-token", token)),
        })
    }

    pub(super) fn gitlab(host: &str, project: &str, token: &str) -> Self {
        Self {
            name: "gitlab",
            url: format!("https://{host}/{project}.git"),
            header: Some(basic("oauth2", token)),
        }
    }

    /// `git` in `root`, authenticated against this remote.
    pub(super) fn git(&self, root: &Path) -> Result<Command> {
        let mut command = command(root);
        if let Some(header) = &self.header {
            let count = env::var("GIT_CONFIG_COUNT").ok();
            command.envs(config_entry(count.as_deref(), "http.extraHeader", header)?);
        }
        Ok(command)
    }

    /// Bring every release tag the remote carries into `root`. A tag sits on
    /// the stamp commit the tag step makes beside the branch, so no branch
    /// fetch ever brings it along.
    pub(super) fn fetch_tags(&self, root: &Path) -> Result<()> {
        output(
            self.git(root)?.args([
                "fetch",
                "--no-tags",
                "--force",
                &self.url,
                "+refs/tags/v*:refs/tags/v*",
            ]),
            None,
        )
        .map(drop)
    }
}

/// The environment that adds `key = value` to git's configuration after the
/// `count` entries the caller's environment already passes the same way — the
/// CI job pins the HTTP version so — which stay in force.
fn config_entry(count: Option<&str>, key: &str, value: &str) -> Result<[(String, String); 3]> {
    let index = count
        .map_or(Ok(0), str::parse::<usize>)
        .with_context(|| format!("GIT_CONFIG_COUNT is not a number: {count:?}"))?;
    Ok([
        ("GIT_CONFIG_COUNT".to_string(), (index + 1).to_string()),
        (format!("GIT_CONFIG_KEY_{index}"), key.to_string()),
        (format!("GIT_CONFIG_VALUE_{index}"), value.to_string()),
    ])
}

fn basic(user: &str, token: &str) -> String {
    format!(
        "Authorization: Basic {}",
        STANDARD.encode(format!("{user}:{token}"))
    )
}

pub(super) fn github_remote(repo: &str) -> Result<String> {
    let valid = regex::Regex::new(r"^[0-9A-Za-z_.-]+/[0-9A-Za-z_.-]+$")
        .context("compile GitHub repository regex")?
        .is_match(repo);
    if !valid {
        bail!("invalid GitHub repository name: {repo:?}");
    }
    Ok(format!("https://github.com/{repo}.git"))
}

/// The token a release step authenticates with.
pub(super) fn token(variable: &str) -> Result<String> {
    env::var(variable)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{variable} is required"))
}

/// `git` in `root`, whatever repository the caller's environment points at:
/// a hook or a job may export one, and the release addresses `root` alone.
pub(super) fn command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
}

/// `git` in `root`, recording commits as the release.
pub(super) fn committer(root: &Path) -> Command {
    let mut command = command(root);
    command.envs(consts::IDENTITY);
    command
}

/// `git args` in `root`, answering with its trimmed output.
pub(super) fn git(root: &Path, args: &[&str]) -> Result<String> {
    output(command(root).args(args), None).map(|text| text.trim().to_string())
}

/// Run `command`, feeding it `input`, and answer with its output. The command
/// is named by its subcommand alone: its environment may carry a token.
pub(super) fn output(command: &mut Command, input: Option<&[u8]>) -> Result<String> {
    let subcommand = command
        .get_args()
        .next()
        .map(|arg| arg.to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = format!("git {subcommand}");
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("running {label}"))?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .with_context(|| format!("{label} has no standard input"))?
            .write_all(input)
            .with_context(|| format!("feeding {label}"))?;
    }
    let result = child
        .wait_with_output()
        .with_context(|| format!("waiting for {label}"))?;
    if !result.status.success() {
        bail!(
            "{label} failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    String::from_utf8(result.stdout).with_context(|| format!("{label} output was not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_remote_is_explicitly_github() {
        assert_eq!(
            github_remote("zvuk/kithara").unwrap(),
            "https://github.com/zvuk/kithara.git"
        );
        assert!(github_remote("zvuk/kithara?token=secret").is_err());
    }

    #[test]
    fn a_token_never_reaches_the_remote_url() {
        let remote = Remote::gitlab("gitlab.example", "group/project", "secret");

        assert_eq!(remote.url, "https://gitlab.example/group/project.git");
        assert_eq!(
            remote.header.as_deref(),
            Some("Authorization: Basic b2F1dGgyOnNlY3JldA=="),
            "oauth2:secret, base64"
        );
    }

    #[test]
    fn the_header_joins_the_configuration_the_job_passes() {
        let entry = |count| config_entry(count, "http.extraHeader", "h").unwrap();

        assert_eq!(
            entry(None),
            [
                ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                (
                    "GIT_CONFIG_KEY_0".to_string(),
                    "http.extraHeader".to_string()
                ),
                ("GIT_CONFIG_VALUE_0".to_string(), "h".to_string()),
            ]
        );
        assert_eq!(entry(Some("1"))[0].1, "2");
        assert_eq!(entry(Some("1"))[1].0, "GIT_CONFIG_KEY_1");
        assert!(config_entry(Some("one"), "k", "v").is_err());
    }
}
