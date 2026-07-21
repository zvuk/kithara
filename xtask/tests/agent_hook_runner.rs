#![cfg(unix)]

use std::{
    env, fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result};
use serde_json::Value;
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
    justfile: PathBuf,
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
        let justfile = repo.join("justfile");

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
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("resolve repository root")?;
        fs::copy(repository.join("justfile"), &justfile).context("copy repository justfile")?;
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
            justfile,
            xtask_log,
        })
    }

    fn cache_dir(&self) -> PathBuf {
        self.git_dir.join("kithara-agent-hook")
    }

    fn cache_pointer(&self) -> PathBuf {
        self.repo.join("xtask/.agent-hook-cache")
    }

    fn installed_generation(&self) -> Result<PathBuf> {
        let body = fs::read_to_string(self.cache_pointer())?;
        Ok(PathBuf::from(body.trim()))
    }

    fn command(&self, hook: &str) -> Command {
        let mut command = Command::new("just");
        command
            .args(["--quiet", "--justfile"])
            .arg(&self.justfile)
            .arg("--working-directory")
            .arg(&self.repo)
            .args(["_agent-hook", hook])
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
        let generation = fs::canonicalize(generation)?;
        write_executable(
            &generation.join("xtask"),
            r#"#!/bin/sh
set -eu
printf 'args=%s\nroot=%s\ncache=%s\n' "$*" "$KITHARA_AGENT_HOOK_ROOT" "$KITHARA_AGENT_HOOK_CACHE" >> "$FAKE_XTASK_LOG"
cat >/dev/null
"#,
        )?;
        fs::write(self.cache_pointer(), format!("{}\n", generation.display()))?;
        Ok(())
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
        assert!(!self.cargo_log.exists(), "Just recipe invoked Cargo");
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

fn collect_adapter_commands(value: &Value, commands: &mut Vec<String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_adapter_commands(value, commands);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if key == "command"
                    && let Some(command) = value.as_str()
                {
                    commands.push(command.to_owned());
                }
                collect_adapter_commands(value, commands);
            }
        }
        _ => {}
    }
}

fn adapter_commands(repository: &Path) -> Result<Vec<String>> {
    let mut commands = Vec::new();
    for adapter in [".claude/settings.json", ".codex/hooks.json"] {
        let value = serde_json::from_slice(&fs::read(repository.join(adapter))?)?;
        collect_adapter_commands(&value, &mut commands);
    }
    Ok(commands)
}

#[test]
fn missing_pointer_skips_hook_without_invoking_cargo() -> Result<()> {
    let fixture = Fixture::new()?;
    let output = fixture.run("pre-bash")?;

    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "warning: agent hook is not installed for this worktree; run `cargo xtask agent-hook install`\n"
    );
    fixture.assert_cargo_not_run();
    assert!(!fixture.cache_dir().exists());
    assert!(!fixture.cache_pointer().exists());
    Ok(())
}

#[test]
fn adapters_fail_open_when_just_is_unavailable() -> Result<()> {
    let fixture = Fixture::new()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("resolve repository root")?;
    let missing = fixture.repo.join("missing-just");
    let unlaunchable = fixture.repo.join("unlaunchable-just");
    fs::create_dir(&missing)?;
    fs::create_dir(&unlaunchable)?;
    write_executable(&unlaunchable.join("just"), "#!/bin/sh\nexit 126\n")?;

    let commands = adapter_commands(repository)?;
    assert_eq!(commands.len(), 3);
    for path in [missing, unlaunchable] {
        for command in &commands {
            let output = Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .current_dir(fixture.repo.join("xtask/src"))
                .env("CLAUDE_PROJECT_DIR", &fixture.repo)
                .env("PATH", &path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .context("run adapter without usable Just")?;

            assert_success(&output);
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                "warning: agent hook launcher is unavailable; install just and retry\n"
            );
        }
    }
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn invalid_pointers_skip_hook_without_invoking_cargo() -> Result<()> {
    for body in [
        "\n",
        "relative-generation\n",
        "/missing-generation\n",
        "/first-generation\n/second-generation\n",
        "/first-generation\n/second-generation",
        "/missing-newline",
    ] {
        let fixture = Fixture::new()?;
        fs::write(fixture.cache_pointer(), body)?;

        let output = fixture.run("pre-bash")?;

        assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains("cargo xtask agent-hook install"));
        assert!(!fixture.xtask_log.exists());
        fixture.assert_cargo_not_run();
    }
    Ok(())
}

#[test]
fn unlaunchable_binary_skips_hook_without_invoking_cargo() -> Result<()> {
    let fixture = Fixture::new()?;
    let generation = fixture.cache_dir().join("generation-unlaunchable");
    fs::create_dir_all(generation.join("xtask"))?;
    let generation = fs::canonicalize(generation)?;
    fs::write(
        fixture.cache_pointer(),
        format!("{}\n", generation.display()),
    )?;

    let output = fixture.run("pre-bash")?;

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("cargo xtask agent-hook install"));
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn installed_pointer_runs_without_reading_git_metadata() -> Result<()> {
    let fixture = Fixture::linked_absolute()?;
    fixture.install_fake_xtask()?;
    fs::write(fixture.repo.join(".git"), "invalid\n")?;

    let output = fixture.run("pre-bash")?;

    assert_success(&output);
    assert!(fixture.xtask_log.is_file());
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn legacy_git_dir_current_is_ignored_without_local_pointer() -> Result<()> {
    let fixture = Fixture::new()?;
    let cache = fixture.cache_dir();
    let generation = cache.join("generation-legacy");
    fs::create_dir_all(&generation)?;
    write_executable(
        &generation.join("xtask"),
        "#!/bin/sh\nprintf legacy >\"$FAKE_XTASK_LOG\"\n",
    )?;
    symlink("generation-legacy", cache.join("current"))?;

    let output = fixture.run("pre-bash")?;

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("cargo xtask agent-hook install"));
    assert!(!fixture.xtask_log.exists());
    fixture.assert_cargo_not_run();
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
    assert!(!fixture.cache_dir().join("current").exists());
    assert!(fixture.cache_pointer().is_file());
    Ok(())
}

#[test]
fn installed_cache_executes_directly_without_invoking_cargo() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.install_fake_xtask()?;

    assert_success(&fixture.run("pre-bash")?);
    assert_success(&fixture.run("post-edit")?);

    fixture.assert_cargo_not_run();
    let root = fs::canonicalize(&fixture.repo)?;
    let generation = fs::canonicalize(fixture.cache_dir().join("generation-test"))?;
    assert_eq!(
        fs::read_to_string(&fixture.xtask_log)?,
        format!(
            "args=agent-hook pre-bash\nroot={}\ncache={}\nargs=agent-hook post-edit\nroot={}\ncache={}\n",
            root.display(),
            generation.display(),
            root.display(),
            generation.display()
        )
    );
    Ok(())
}

#[test]
fn explicit_install_publishes_generation_pointer_for_recipe() -> Result<()> {
    let fixture = Fixture::new()?;
    let install = fixture.install_real_xtask()?;
    assert_success(&install);

    let generation = fixture.installed_generation()?;
    let binary = generation.join("xtask");
    let cache = fs::canonicalize(fixture.cache_dir())?;
    assert!(generation.is_absolute());
    assert!(generation.starts_with(cache));
    assert!(binary.is_file());
    assert_ne!(fs::metadata(&binary)?.permissions().mode() & 0o111, 0);
    assert!(generation.join("fingerprint").is_file());
    assert_eq!(
        fs::read_to_string(fixture.cache_pointer())?,
        format!("{}\n", generation.display())
    );
    assert!(!fixture.cache_dir().join("current").exists());

    let output = fixture.run_with_input(
        "pre-bash",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    )?;
    assert_success(&output);
    let response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(response["hookSpecificOutput"]["permissionDecision"], "deny");
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn installed_recipe_cannot_install_itself() -> Result<()> {
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
    let response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(response["hookSpecificOutput"]["permissionDecision"], "deny");
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn justfile_change_does_not_stale_installed_policy() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.install_real_xtask()?);
    let mut justfile = fs::OpenOptions::new()
        .append(true)
        .open(&fixture.justfile)?;
    writeln!(justfile, "# launcher-only change")?;

    let output = fixture.run_with_input(
        "pre-bash",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    )?;

    assert_success(&output);
    assert!(output.stderr.is_empty());
    let response: Value = serde_json::from_slice(&output.stdout)?;
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
    let absolute_generation = absolute.installed_generation()?;
    let relative_generation = relative.installed_generation()?;
    let absolute_cache = fs::canonicalize(absolute.cache_dir())?;
    let relative_cache = fs::canonicalize(relative.cache_dir())?;
    assert_ne!(absolute_generation, relative_generation);
    assert!(absolute_generation.starts_with(absolute_cache));
    assert!(relative_generation.starts_with(relative_cache));
    assert!(absolute_generation.join("xtask").is_file());
    assert!(relative_generation.join("xtask").is_file());
    assert!(absolute_generation.join("fingerprint").is_file());
    assert!(relative_generation.join("fingerprint").is_file());
    absolute.assert_cargo_not_run();
    relative.assert_cargo_not_run();
    Ok(())
}

#[test]
fn reinstall_switches_pointer_between_complete_generations() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.install_real_xtask()?);
    let previous = fixture.installed_generation()?;
    fs::write(
        fixture.repo.join("xtask/src/agent_hook.rs"),
        "pub fn run() { println!(\"changed\"); }\n",
    )?;

    assert_success(&fixture.install_real_xtask()?);

    let current = fixture.installed_generation()?;
    assert_ne!(previous, current);
    for generation in [previous, current] {
        assert!(generation.join("xtask").is_file());
        assert!(generation.join("fingerprint").is_file());
    }
    Ok(())
}

#[test]
fn just_recipe_contains_no_bootstrap_build_logic() -> Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("resolve repository root")?;
    let body = fs::read_to_string(repository.join("justfile"))?;
    let recipe = body
        .split_once("_agent-hook HOOK:\n")
        .context("find agent hook recipe")?
        .1
        .split_once("# --- formatting ---")
        .context("find end of agent hook section")?
        .0;

    for forbidden in [
        ".git",
        "gitdir:",
        "kithara-agent-hook",
        "current",
        "cargo build",
        "lockf",
        "flock",
        "readlink",
        "setsid",
        "shasum",
        "CARGO_TARGET_DIR",
    ] {
        assert!(
            !recipe.contains(forbidden),
            "agent hook recipe contains {forbidden}"
        );
    }
    Ok(())
}

#[test]
fn agent_hook_recipe_is_hidden_from_just_list() -> Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("resolve repository root")?;
    let output = Command::new("just")
        .args(["--justfile"])
        .arg(repository.join("justfile"))
        .arg("--list")
        .output()
        .context("list public Just recipes")?;

    assert_success(&output);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("_agent-hook"));
    Ok(())
}

#[test]
fn launcher_is_owned_only_by_justfile() -> Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("resolve repository root")?;
    let body = fs::read_to_string(repository.join("justfile"))?;

    assert!(body.contains("_agent-hook HOOK:"));
    assert!(!repository.join("xtask/agent-hook").exists());
    Ok(())
}
