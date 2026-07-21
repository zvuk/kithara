#![cfg(unix)]

use std::{
    env, fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result};
use tempfile::TempDir;

#[derive(Clone, Copy)]
enum GitLayout {
    Directory,
    LinkedAbsolute,
    LinkedRelative,
}

struct Fixture {
    _temp: TempDir,
    cargo_log: PathBuf,
    git_dir: PathBuf,
    path: std::ffi::OsString,
    repo: PathBuf,
    runner: PathBuf,
    xtask_log: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        Self::with_git_layout(GitLayout::Directory)
    }

    fn linked_absolute() -> Result<Self> {
        Self::with_git_layout(GitLayout::LinkedAbsolute)
    }

    fn linked_relative() -> Result<Self> {
        Self::with_git_layout(GitLayout::LinkedRelative)
    }

    fn with_git_layout(layout: GitLayout) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path().join("repo");
        let git_dir = match layout {
            GitLayout::Directory => repo.join(".git"),
            GitLayout::LinkedAbsolute | GitLayout::LinkedRelative => {
                temp.path().join("git/worktrees/repo")
            }
        };
        let fake_bin = temp.path().join("bin");
        let cargo_log = temp.path().join("cargo.log");
        let xtask_log = temp.path().join("xtask.log");
        let runner = repo.join("xtask/agent-hook");

        fs::create_dir_all(&git_dir)?;
        fs::create_dir_all(repo.join("xtask/src/agent_hook"))?;
        fs::create_dir_all(&fake_bin)?;
        match layout {
            GitLayout::Directory => {}
            GitLayout::LinkedAbsolute => {
                fs::write(
                    repo.join(".git"),
                    format!("gitdir: {}\n", git_dir.display()),
                )?;
            }
            GitLayout::LinkedRelative => {
                fs::write(repo.join(".git"), "gitdir: ../git/worktrees/repo\n")?;
            }
        }
        fs::write(
            repo.join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
        )?;
        fs::write(repo.join("xtask/src/main.rs"), "fn main() {}\n")?;
        fs::write(repo.join("xtask/src/agent_hook.rs"), "pub fn run() {}\n")?;
        fs::write(
            repo.join("xtask/src/agent_hook/input.rs"),
            "pub fn read() {}\n",
        )?;
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("agent-hook"),
            &runner,
        )
        .context("copy xtask agent-hook runner")?;
        make_executable(&runner)?;
        write_executable(
            &fake_bin.join("cargo"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$FAKE_CARGO_LOG"
exit 91
"#,
        )?;

        let mut paths = vec![fake_bin.clone()];
        if let Some(system_path) = env::var_os("PATH") {
            paths.extend(env::split_paths(&system_path));
        }
        let path = env::join_paths(paths).context("construct fixture PATH")?;

        Ok(Self {
            _temp: temp,
            cargo_log,
            git_dir,
            path,
            repo,
            runner,
            xtask_log,
        })
    }

    fn cache_dir(&self) -> PathBuf {
        self.git_dir.join("kithara-agent-hook")
    }

    fn command(&self, hook: &str) -> Command {
        let mut command = Command::new(&self.runner);
        command
            .arg(hook)
            .current_dir(self.repo.join("xtask/src"))
            .env("FAKE_CARGO_LOG", &self.cargo_log)
            .env("FAKE_XTASK_LOG", &self.xtask_log)
            .env("PATH", &self.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, hook: &str) -> Result<Output> {
        self.command(hook)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("run agent hook command {hook}"))
    }

    fn run_with_input(&self, hook: &str, input: &str) -> Result<Output> {
        let mut child = self.command(hook).stdin(Stdio::piped()).spawn()?;
        child
            .stdin
            .take()
            .context("open hook stdin")?
            .write_all(input.as_bytes())?;
        child.wait_with_output().context("wait for agent hook")
    }

    fn install_fake_xtask(&self) -> Result<()> {
        let cache = self.cache_dir();
        let generation = cache.join("generation-test");
        fs::create_dir_all(&generation)?;
        symlink("generation-test", cache.join("current"))?;
        write_executable(
            &generation.join("xtask"),
            r#"#!/bin/sh
set -eu
printf 'args=%s\nroot=%s\ncache=%s\n' "$*" "$KITHARA_AGENT_HOOK_ROOT" "$KITHARA_AGENT_HOOK_CACHE" >> "$FAKE_XTASK_LOG"
cat >/dev/null
"#,
        )
    }

    fn install_real_xtask(&self) -> Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["agent-hook", "install"])
            .current_dir(&self.repo)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("install cached xtask")
    }

    fn assert_cargo_not_run(&self) {
        assert!(!self.cargo_log.exists(), "runner invoked Cargo");
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn make_executable(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn write_executable(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    make_executable(path)
}

#[test]
fn missing_cache_skips_hook_without_invoking_cargo() -> Result<()> {
    let fixture = Fixture::new()?;
    let output = fixture.run("pre-bash")?;

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("cargo xtask agent-hook install"));
    fixture.assert_cargo_not_run();
    assert!(!fixture.cache_dir().exists());
    Ok(())
}

#[test]
fn legacy_flat_cache_is_ignored() -> Result<()> {
    let fixture = Fixture::new()?;
    fs::create_dir_all(fixture.cache_dir())?;
    write_executable(
        &fixture.cache_dir().join("xtask"),
        "#!/bin/sh\nprintf legacy >\"$FAKE_XTASK_LOG\"\n",
    )?;
    fs::write(fixture.cache_dir().join("fingerprint"), "legacy\n")?;

    let output = fixture.run("pre-bash")?;

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("cargo xtask agent-hook install"));
    assert!(!fixture.xtask_log.exists());
    fixture.assert_cargo_not_run();

    assert_success(&fixture.install_real_xtask()?);
    assert!(!fixture.cache_dir().join("xtask").exists());
    assert!(!fixture.cache_dir().join("fingerprint").exists());
    assert!(fixture.cache_dir().join("current/xtask").is_file());
    Ok(())
}

#[test]
fn installed_cache_executes_directly_without_invoking_cargo() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.install_fake_xtask()?;

    assert_success(&fixture.run("pre-bash")?);
    assert_success(&fixture.run("post-edit")?);

    fixture.assert_cargo_not_run();
    let generation = fs::canonicalize(fixture.cache_dir().join("generation-test"))?;
    assert_eq!(
        fs::read_to_string(&fixture.xtask_log)?,
        format!(
            "args=agent-hook pre-bash\nroot={}\ncache={}\nargs=agent-hook post-edit\nroot={}\ncache={}\n",
            fixture.repo.display(),
            generation.display(),
            fixture.repo.display(),
            generation.display()
        )
    );
    Ok(())
}

#[test]
fn explicit_install_publishes_current_xtask_for_the_runner() -> Result<()> {
    let fixture = Fixture::new()?;
    let install = fixture.install_real_xtask()?;
    assert_success(&install);

    let binary = fixture.cache_dir().join("current/xtask");
    assert!(binary.is_file());
    assert_ne!(fs::metadata(&binary)?.permissions().mode() & 0o111, 0);
    assert!(fixture.cache_dir().join("current/fingerprint").is_file());

    let output = fixture.run_with_input(
        "pre-bash",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    )?;
    assert_success(&output);
    let response: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(response["hookSpecificOutput"]["permissionDecision"], "deny");
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn installed_runner_cannot_mark_itself_current() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.install_real_xtask()?);

    let output = fixture.run("install")?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cargo xtask agent-hook install"));
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn stale_cache_warns_and_runs_last_good_policy_without_cargo() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.install_real_xtask()?);
    fs::write(
        fixture.repo.join("xtask/src/agent_hook.rs"),
        "pub fn run() { println!(\"changed\"); }\n",
    )?;

    let output = fixture.run_with_input(
        "pre-bash",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    )?;

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("cached agent hook is stale"));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(response["hookSpecificOutput"]["permissionDecision"], "deny");
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn manifest_post_edit_never_invokes_cargo() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.install_real_xtask()?);
    let manifest = fixture.repo.join("xtask/Cargo.toml");
    let before = fs::read(&manifest)?;
    let input = serde_json::json!({
        "tool_name": "Edit",
        "tool_input": {
            "file_path": manifest
        }
    })
    .to_string();

    let output = fixture.run_with_input("post-edit", &input)?;

    assert_success(&output);
    assert_eq!(before, fs::read(fixture.repo.join("xtask/Cargo.toml"))?);
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn linked_worktrees_use_distinct_installed_caches() -> Result<()> {
    let absolute = Fixture::linked_absolute()?;
    let relative = Fixture::linked_relative()?;
    assert_success(&absolute.install_real_xtask()?);
    assert_success(&relative.install_real_xtask()?);

    assert_success(&absolute.run("pre-bash")?);
    assert_success(&relative.run("pre-bash")?);

    assert_ne!(absolute.cache_dir(), relative.cache_dir());
    assert!(absolute.cache_dir().join("current/xtask").is_file());
    assert!(relative.cache_dir().join("current/xtask").is_file());
    assert!(absolute.cache_dir().join("current/fingerprint").is_file());
    assert!(relative.cache_dir().join("current/fingerprint").is_file());
    absolute.assert_cargo_not_run();
    relative.assert_cargo_not_run();
    Ok(())
}

#[test]
fn runner_stays_thin_and_contains_no_bootstrap_build_logic() -> Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("agent-hook");
    let body = fs::read_to_string(path)?;

    assert!(body.lines().count() <= 50);
    for forbidden in [
        "cargo build",
        "lockf",
        "flock",
        "setsid",
        "shasum",
        "CARGO_TARGET_DIR",
    ] {
        assert!(!body.contains(forbidden), "runner contains {forbidden}");
    }
    Ok(())
}

#[test]
fn runner_is_checked_in_as_executable() -> Result<()> {
    let runner = Path::new(env!("CARGO_MANIFEST_DIR")).join("agent-hook");
    let mode = fs::metadata(runner)?.permissions().mode();

    assert_ne!(mode & 0o111, 0);
    Ok(())
}
