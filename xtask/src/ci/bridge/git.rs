use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};

use super::{
    api::{Github, Gitlab},
    command::BridgeConfig,
    control::{ControlChange, classify},
    model::CONTROL_PATHS,
};

pub(super) struct GitRepo {
    root: PathBuf,
    config: BridgeConfig,
}

impl GitRepo {
    pub(super) fn new(state_dir: &Path, config: &BridgeConfig) -> Result<Self> {
        let root = state_dir.join("repository.git");
        if !root.exists() {
            fs::create_dir_all(state_dir)
                .with_context(|| format!("creating bridge state {}", state_dir.display()))?;
            let output = Command::new("git")
                .current_dir(state_dir)
                .args(["init", "--bare"])
                .arg(&root)
                .output()
                .context("initializing bridge bare repository")?;
            checked(output, "git init --bare")?;
        }
        Ok(Self {
            root,
            config: config.clone(),
        })
    }

    pub(super) fn fetch(&self, github: &Github, gitlab: &Gitlab) -> Result<()> {
        let github_url = format!("https://github.com/{}.git", self.config.github_repo);
        self.run(
            &[
                "fetch",
                "--no-tags",
                "--force",
                &github_url,
                &format!(
                    "+refs/heads/{}:refs/heads/bridge/github",
                    self.config.branch
                ),
            ],
            Some(github.git_header()),
        )?;

        let gitlab_url = format!(
            "{}/{}.git",
            self.config.gitlab_origin(),
            self.config.gitlab_project_path
        );
        self.run(
            &[
                "fetch",
                "--no-tags",
                "--force",
                &gitlab_url,
                &format!(
                    "+refs/heads/{}:refs/heads/bridge/gitlab",
                    self.config.branch
                ),
            ],
            Some(gitlab.git_header()),
        )?;
        Ok(())
    }

    pub(super) fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool> {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(["merge-base", "--is-ancestor", older, newer])
            .output()
            .context("running git merge-base --is-ancestor")?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            code => bail!(
                "git merge-base failed with exit code {}: {}",
                code.unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
    }

    pub(super) fn fetch_pull_head(
        &self,
        github: &Github,
        pull_number: u64,
        expected_sha: &str,
    ) -> Result<()> {
        let github_url = format!("https://github.com/{}.git", self.config.github_repo);
        let reference = format!("refs/heads/bridge/pull/{pull_number}");
        self.run(
            &[
                "fetch",
                "--no-tags",
                "--force",
                &github_url,
                &format!("+refs/pull/{pull_number}/head:{reference}"),
            ],
            Some(github.git_header()),
        )?;
        let fetched = self.rev_parse(&reference)?;
        if fetched != expected_sha {
            bail!(
                "GitHub PR #{pull_number} API head {expected_sha} does not match fetched head {fetched}"
            );
        }
        Ok(())
    }

    pub(super) fn changed_control_paths(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args([
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                base,
                head,
                "--",
            ])
            .args(CONTROL_PATHS)
            .output()
            .context("running git diff for CI control paths")?;
        let paths = String::from_utf8(checked(output, "git diff for CI control paths")?)
            .context("git diff returned a non-UTF-8 control path")?;
        Ok(paths
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// The subset of [`Self::changed_control_paths`] that does not survive the
    /// additive test: an entry the judge already had was edited or removed.
    /// Adding a lane beside the existing ones is not one of those, so it no
    /// longer costs the author a separate merge request.
    pub(super) fn weakening_control_paths(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let mut weakening = Vec::new();
        for path in self.changed_control_paths(base, head)? {
            let before = self.file_at(base, &path)?;
            let after = self.file_at(head, &path)?;
            if classify(&path, before.as_deref(), after.as_deref()) == ControlChange::Weakening {
                weakening.push(path);
            }
        }
        Ok(weakening)
    }

    /// Content of `path` at `reference`, or `None` when it is absent there.
    /// Non-UTF-8 content reads as absent on purpose: the classifier cannot
    /// judge bytes it cannot compare, and the caller treats that as weakening.
    fn file_at(&self, reference: &str, path: &str) -> Result<Option<String>> {
        if !self.exists_at(reference, path)? {
            return Ok(None);
        }
        let bytes = self.run(&["show", &format!("{reference}:{path}")], None)?;
        Ok(String::from_utf8(bytes).ok())
    }

    pub(super) fn judged_commit(&self, base: &str, head: &str) -> Result<String> {
        let tree = self.root.join("quarantine-worktree");
        self.discard_worktree(&tree)?;
        self.run(
            &[
                "worktree",
                "add",
                "--detach",
                "--force",
                path_text(&tree)?,
                head,
            ],
            None,
        )?;
        let outcome = self.restore_control_paths(&tree, base);
        let commit = outcome.and_then(|()| Self::commit_worktree(&tree, head));
        self.discard_worktree(&tree)?;
        commit
    }

    fn restore_control_paths(&self, tree: &Path, base: &str) -> Result<()> {
        for path in CONTROL_PATHS {
            let path = path.trim_end_matches('/');
            if self.exists_at(base, path)? {
                git_in(tree, &["checkout", base, "--", path])?;
            } else {
                git_in(
                    tree,
                    &["rm", "-r", "--force", "--ignore-unmatch", "--", path],
                )?;
            }
        }
        Ok(())
    }

    fn commit_worktree(tree: &Path, head: &str) -> Result<String> {
        git_in(tree, &["add", "--all"])?;
        if git_in(tree, &["diff", "--cached", "--quiet"]).is_ok() {
            return Ok(head.to_owned());
        }
        let message = format!("quarantine: {head} judged with trusted CI");
        git_in_with_date(
            tree,
            &[
                "-c",
                "user.name=kithara-bridge",
                "-c",
                "user.email=kithara-bridge@localhost",
                "commit",
                "--quiet",
                "--message",
                &message,
            ],
        )?;
        let sha = git_in(tree, &["rev-parse", "HEAD"])?;
        Ok(String::from_utf8(sha)
            .context("git rev-parse returned invalid UTF-8")?
            .trim()
            .to_owned())
    }

    fn discard_worktree(&self, tree: &Path) -> Result<()> {
        if tree.exists() {
            let _ = self.run(&["worktree", "remove", "--force", path_text(tree)?], None);
            if tree.exists() {
                fs::remove_dir_all(tree).with_context(|| format!("removing {}", tree.display()))?;
            }
        }
        let _ = self.run(&["worktree", "prune"], None);
        Ok(())
    }

    fn exists_at(&self, reference: &str, path: &str) -> Result<bool> {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(["cat-file", "-e", &format!("{reference}:{path}")])
            .output()
            .context("running git cat-file for a control path")?;
        Ok(output.status.success())
    }

    fn rev_parse(&self, reference: &str) -> Result<String> {
        let bytes = self.run(&["rev-parse", reference], None)?;
        Ok(String::from_utf8(bytes)
            .context("git rev-parse returned invalid UTF-8")?
            .trim()
            .to_owned())
    }

    pub(super) fn push_gitlab(&self, gitlab: &Gitlab, sha: &str, destination: &str) -> Result<()> {
        let url = format!(
            "{}/{}.git",
            self.config.gitlab_origin(),
            self.config.gitlab_project_path
        );
        self.run(
            &["push", &url, &format!("{sha}:refs/heads/{destination}")],
            Some(gitlab.git_header()),
        )?;
        Ok(())
    }

    pub(super) fn push_github(&self, github: &Github, sha: &str) -> Result<()> {
        let url = format!("https://github.com/{}.git", self.config.github_repo);
        self.run(
            &[
                "push",
                &url,
                &format!("{sha}:refs/heads/{}", self.config.branch),
            ],
            Some(github.git_header()),
        )?;
        Ok(())
    }

    fn run(&self, args: &[&str], header: Option<String>) -> Result<Vec<u8>> {
        let mut command = Command::new("git");
        command.current_dir(&self.root).args(args);
        if let Some(header) = header {
            command
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "http.extraHeader")
                .env("GIT_CONFIG_VALUE_0", header);
        }
        let output = command
            .output()
            .with_context(|| format!("running git {}", args.first().unwrap_or(&"<unknown>")))?;
        checked(
            output,
            &format!("git {}", args.first().unwrap_or(&"<unknown>")),
        )
    }
}

fn checked(output: Output, label: &str) -> Result<Vec<u8>> {
    if !output.status.success() {
        bail!(
            "{label} failed with exit code {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
}

fn git_in(tree: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(tree)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.first().unwrap_or(&"<unknown>")))?;
    checked(
        output,
        &format!("git {}", args.first().unwrap_or(&"<unknown>")),
    )
}

fn git_in_with_date(tree: &Path, args: &[&str]) -> Result<Vec<u8>> {
    const EPOCH: &str = "1970-01-01T00:00:00Z";
    let output = Command::new("git")
        .current_dir(tree)
        .env("GIT_AUTHOR_DATE", EPOCH)
        .env("GIT_COMMITTER_DATE", EPOCH)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.first().unwrap_or(&"<unknown>")))?;
    checked(
        output,
        &format!("git {}", args.first().unwrap_or(&"<unknown>")),
    )
}

#[cfg(test)]
mod tests {
    use reqwest::Url;
    use tempfile::TempDir;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("running git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn config(state: &Path) -> BridgeConfig {
        BridgeConfig {
            github_repo: "owner/repo".into(),
            github_token_file: state.join("github.token"),
            gitlab_url: Url::parse("https://gitlab.example.com").unwrap(),
            gitlab_project_id: 1,
            gitlab_project_path: "group/repo".into(),
            gitlab_username: "bot".into(),
            gitlab_token_file: state.join("gitlab.token"),
            branch: "main".into(),
            state_dir: state.to_path_buf(),
        }
    }

    fn repository() -> (TempDir, GitRepo, String, String, String, String) {
        let state = TempDir::new().unwrap();
        let repo = GitRepo::new(state.path(), &config(state.path())).unwrap();
        let work = state.path().join("work");
        fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        git(&work, &["config", "user.email", "t@e.st"]);
        git(&work, &["config", "user.name", "test"]);
        fs::create_dir_all(work.join("xtask/src")).unwrap();
        fs::create_dir_all(work.join(".config")).unwrap();
        fs::write(work.join("xtask/src/main.rs"), "// trusted\n").unwrap();
        fs::write(work.join(".gitlab-ci.yml"), "trusted\n").unwrap();
        fs::write(
            work.join(".config/xtask.toml"),
            "[test.lanes.loom]\nfeatures = [\"loom\"]\n",
        )
        .unwrap();
        fs::write(work.join("src.rs"), "base\n").unwrap();
        git(&work, &["add", "--all"]);
        git(&work, &["commit", "--quiet", "-m", "base"]);
        let base = String::from_utf8(git_in(&work, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned();

        git(&work, &["checkout", "--quiet", "-b", "additive"]);
        fs::write(
            work.join(".config/xtask.toml"),
            "[test.lanes.loom]\nfeatures = [\"loom\"]\n\n[test.lanes.broadcast]\nfeatures = [\"broadcast\"]\n",
        )
        .unwrap();
        git(&work, &["add", "--all"]);
        git(
            &work,
            &["commit", "--quiet", "-m", "a lane beside the others"],
        );
        let additive = String::from_utf8(git_in(&work, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned();
        git(&work, &["checkout", "--quiet", "main"]);

        fs::write(work.join("src.rs"), "product\n").unwrap();
        git(&work, &["add", "--all"]);
        git(&work, &["commit", "--quiet", "-m", "product"]);
        let product = String::from_utf8(git_in(&work, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned();

        fs::write(work.join("xtask/src/main.rs"), "// from patch\n").unwrap();
        fs::write(work.join(".gitlab-ci.yml"), "from patch\n").unwrap();
        git(&work, &["add", "--all"]);
        git(&work, &["commit", "--quiet", "-m", "control"]);
        let head = String::from_utf8(git_in(&work, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned();
        git(
            &work,
            &[
                "push",
                "--quiet",
                repo.root.to_str().unwrap(),
                "main",
                "additive",
            ],
        );
        (state, repo, base, product, head, additive)
    }

    fn blob(repo: &GitRepo, reference: &str, path: &str) -> String {
        String::from_utf8(
            repo.run(&["show", &format!("{reference}:{path}")], None)
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn judged_commit_combines_product_head_with_trusted_base_controls() {
        let (_state, repo, base, _product, head, _additive) = repository();
        let judged = repo.judged_commit(&base, &head).unwrap();

        assert_eq!(blob(&repo, &judged, "src.rs"), "product\n");
        assert_eq!(blob(&repo, &judged, ".gitlab-ci.yml"), "trusted\n");
        assert_eq!(blob(&repo, &judged, "xtask/src/main.rs"), "// trusted\n");
    }

    #[test]
    fn judged_commit_is_deterministic_for_the_exact_key() {
        let (_state, repo, base, _product, head, _additive) = repository();
        assert_eq!(
            repo.judged_commit(&base, &head).unwrap(),
            repo.judged_commit(&base, &head).unwrap()
        );
    }

    #[test]
    fn product_only_head_has_no_control_changes_and_needs_no_overlay_commit() {
        let (_state, repo, base, product, _head, _additive) = repository();

        assert!(
            repo.changed_control_paths(&base, &product)
                .unwrap()
                .is_empty()
        );
        assert_eq!(repo.judged_commit(&base, &product).unwrap(), product);
    }

    #[test]
    fn control_path_changes_are_detected_before_judging() {
        let (_state, repo, base, _product, head, _additive) = repository();
        let paths = repo.changed_control_paths(&base, &head).unwrap();

        assert_eq!(paths, [".gitlab-ci.yml", "xtask/src/main.rs"]);
    }

    /// The shape a pull request has when it adds a lane: the control file
    /// changed, but every lane that already judged is still there, unedited.
    /// Such a change no longer costs its author a separate merge request.
    #[test]
    fn a_lane_added_beside_the_others_does_not_weaken_the_judge() {
        let (_state, repo, base, _product, _head, additive) = repository();

        assert_eq!(
            repo.changed_control_paths(&base, &additive).unwrap(),
            [".config/xtask.toml"]
        );
        assert!(
            repo.weakening_control_paths(&base, &additive)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rewriting_a_control_file_weakens_the_judge() {
        let (_state, repo, base, _product, head, _additive) = repository();

        assert_eq!(
            repo.weakening_control_paths(&base, &head).unwrap(),
            [".gitlab-ci.yml", "xtask/src/main.rs"]
        );
    }
}
