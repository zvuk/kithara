#![cfg(unix)]

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
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
    cargo_jobs_log: PathBuf,
    cargo_log: PathBuf,
    cargo_ready: PathBuf,
    child_pid: PathBuf,
    fake_bin: PathBuf,
    fake_xtask: PathBuf,
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
        let cargo_jobs_log = temp.path().join("cargo-jobs.log");
        let cargo_log = temp.path().join("cargo.log");
        let cargo_ready = temp.path().join("cargo.ready");
        let child_pid = temp.path().join("child.pid");
        let xtask_log = temp.path().join("xtask.log");
        let fake_xtask = fake_bin.join("xtask-built");
        let runner = repo.join("xtask/agent-hook");

        fs::create_dir_all(&git_dir)?;
        fs::create_dir_all(&repo)?;
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
        fs::create_dir_all(repo.join("xtask/src"))?;
        fs::create_dir_all(&fake_bin)?;
        fs::write(
            repo.join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
        )?;
        fs::write(repo.join("xtask/src/main.rs"), "fn main() {}\n")?;
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
printf '%s\n' "${CARGO_BUILD_JOBS-}" >> "$FAKE_CARGO_JOBS_LOG"
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$FAKE_XTASK_SOURCE" "$CARGO_TARGET_DIR/debug/xtask"
chmod 755 "$CARGO_TARGET_DIR/debug/xtask"
"#,
        )?;
        write_executable(
            &fake_xtask,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$FAKE_XTASK_LOG"
cat >/dev/null
"#,
        )?;

        let mut paths = vec![fake_bin.clone()];
        if let Some(system_path) = env::var_os("PATH") {
            paths.extend(env::split_paths(&system_path));
        }
        let path = env::join_paths(paths).context("construct fixture PATH")?;

        Ok(Self {
            _temp: temp,
            cargo_jobs_log,
            cargo_log,
            cargo_ready,
            child_pid,
            fake_bin,
            fake_xtask,
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

    fn fail_cargo(&self) -> Result<()> {
        write_executable(
            &self.fake_bin.join("cargo"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$FAKE_CARGO_LOG"
mkdir -p "$CARGO_TARGET_DIR"
printf 'partial\n' > "$CARGO_TARGET_DIR/partial"
exit 77
"#,
        )
    }

    fn command(&self, hook: &str) -> Command {
        let mut command = Command::new(&self.runner);
        command.arg(hook);
        self.configure(&mut command);
        command
    }

    fn command_with_shell(&self, shell: &Path, hook: &str) -> Command {
        let mut command = Command::new(shell);
        command.arg(&self.runner).arg(hook);
        self.configure(&mut command);
        command
    }

    fn configure(&self, command: &mut Command) {
        command
            .current_dir(self.repo.join("xtask/src"))
            .env("FAKE_CARGO_JOBS_LOG", &self.cargo_jobs_log)
            .env("FAKE_CARGO_LOG", &self.cargo_log)
            .env("FAKE_CARGO_READY", &self.cargo_ready)
            .env("FAKE_CHILD_PID", &self.child_pid)
            .env("FAKE_XTASK_LOG", &self.xtask_log)
            .env("FAKE_XTASK_SOURCE", &self.fake_xtask)
            .env("PATH", &self.path)
            .env_remove("CARGO_BUILD_JOBS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }

    fn run(&self, hook: &str) -> Result<Output> {
        self.command(hook)
            .output()
            .with_context(|| format!("run agent hook command {hook}"))
    }

    fn hang_cargo_with_child(&self) -> Result<()> {
        write_executable(
            &self.fake_bin.join("cargo"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$FAKE_CARGO_LOG"
printf '%s\n' "${CARGO_BUILD_JOBS-}" >> "$FAKE_CARGO_JOBS_LOG"
(
    trap '' TERM
    while :; do
        sleep 1
    done
) &
child=$!
printf '%s\n' "$child" > "$FAKE_CHILD_PID"
printf 'ready\n' > "$FAKE_CARGO_READY"
wait "$child"
"#,
        )
    }

    fn install_setsid_shim(&self) -> Result<()> {
        write_executable(
            &self.fake_bin.join("setsid"),
            r#"#!/bin/sh
exec perl -MPOSIX=setsid -e 'my $sid = setsid(); die "setsid: $!" if !defined($sid) || $sid < 0; exec @ARGV or die "exec: $!";' "$@"
"#,
        )
    }
}

struct ProcessGuard(u32);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = signal(self.0, "-KILL");
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "runner failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn line_count(path: &Path) -> Result<usize> {
    Ok(fs::read_to_string(path)?.lines().count())
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

fn signal(pid: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .args([signal, &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "signal {signal} failed for pid {pid}");
    Ok(())
}

fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.is_file() {
        anyhow::ensure!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn wait_for_process_exit(pid: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) {
        anyhow::ensure!(
            Instant::now() < deadline,
            "pid {pid} survived runner cleanup"
        );
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn find_program(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|path| path.join(name))
            .find(|path| path.is_file())
    })
}

fn assert_interrupted_bootstrap_cleanup(fixture: &Fixture, mut command: Command) -> Result<()> {
    fixture.hang_cargo_with_child()?;
    let runner = command.spawn()?;

    wait_for_file(&fixture.cargo_ready)?;
    wait_for_file(&fixture.child_pid)?;
    let child_pid = fs::read_to_string(&fixture.child_pid)?
        .trim()
        .parse::<u32>()?;
    let _guard = ProcessGuard(child_pid);
    signal(runner.id(), "-TERM")?;
    let output = runner.wait_with_output()?;

    assert!(!output.status.success());
    wait_for_process_exit(child_pid)?;
    assert!(!fixture.cache_dir().join("target").exists());
    assert!(!fixture.cache_dir().join("xtask").exists());
    Ok(())
}

#[test]
fn runner_builds_full_xtask_once_and_executes_cached_hook() -> Result<()> {
    let fixture = Fixture::new()?;

    assert_success(&fixture.run("pre-bash")?);
    assert_success(&fixture.run("post-edit")?);

    assert_eq!(line_count(&fixture.cargo_log)?, 1);
    assert_eq!(fs::read_to_string(&fixture.cargo_jobs_log)?, "4\n");
    assert_eq!(
        fs::read_to_string(&fixture.cargo_log)?,
        format!(
            "build --locked --manifest-path {}/Cargo.toml -p xtask --bin xtask\n",
            fixture.repo.display()
        )
    );
    assert_eq!(
        fs::read_to_string(&fixture.xtask_log)?,
        "agent-hook pre-bash\nagent-hook post-edit\n"
    );
    assert!(fixture.cache_dir().join("xtask").is_file());
    assert!(!fixture.cache_dir().join("target").exists());
    Ok(())
}

#[test]
fn runner_preserves_explicit_cargo_jobs() -> Result<()> {
    let fixture = Fixture::new()?;
    let output = fixture
        .command("pre-bash")
        .env("CARGO_BUILD_JOBS", "2")
        .output()?;

    assert_success(&output);
    assert_eq!(fs::read_to_string(&fixture.cargo_jobs_log)?, "2\n");
    Ok(())
}

#[test]
fn interrupted_bootstrap_terminates_build_descendants() -> Result<()> {
    let fixture = Fixture::new()?;
    let command = fixture.command("pre-bash");
    assert_interrupted_bootstrap_cleanup(&fixture, command)
}

#[test]
fn dash_bootstrap_uses_setsid_process_group_when_available() -> Result<()> {
    let Some(dash) = find_program("dash") else {
        return Ok(());
    };
    let fixture = Fixture::new()?;
    if find_program("setsid").is_none() {
        if find_program("perl").is_none() {
            return Ok(());
        }
        fixture.install_setsid_shim()?;
    }
    let command = fixture.command_with_shell(&dash, "pre-bash");
    assert_interrupted_bootstrap_cleanup(&fixture, command)
}

#[test]
fn concurrent_cold_starts_publish_one_xtask() -> Result<()> {
    let fixture = Fixture::new()?;

    let first = fixture.command("pre-bash").spawn()?;
    let second = fixture.command("pre-bash").spawn()?;
    let first = first.wait_with_output()?;
    let second = second.wait_with_output()?;

    assert_success(&first);
    assert_success(&second);
    assert_eq!(line_count(&fixture.cargo_log)?, 1);
    assert_eq!(line_count(&fixture.xtask_log)?, 2);
    Ok(())
}

#[test]
fn linked_worktrees_own_distinct_xtask_caches() -> Result<()> {
    let absolute = Fixture::linked_absolute()?;
    let relative = Fixture::linked_relative()?;

    assert_success(&absolute.run("pre-bash")?);
    assert_success(&relative.run("pre-bash")?);

    assert_ne!(absolute.cache_dir(), relative.cache_dir());
    assert!(absolute.cache_dir().join("xtask").is_file());
    assert!(relative.cache_dir().join("xtask").is_file());
    assert!(!absolute.repo.join(".git/kithara-agent-hook").exists());
    assert!(!relative.repo.join(".git/kithara-agent-hook").exists());
    Ok(())
}

#[test]
fn runner_is_checked_in_as_executable() -> Result<()> {
    let runner = Path::new(env!("CARGO_MANIFEST_DIR")).join("agent-hook");
    let mode = fs::metadata(runner)?.permissions().mode();

    assert_ne!(mode & 0o111, 0);
    Ok(())
}

#[test]
fn stale_post_edit_uses_last_good_xtask() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.run("pre-bash")?);

    fs::write(
        fixture.repo.join("xtask/src/main.rs"),
        "fn main() { println!(\"changed\"); }\n",
    )?;
    fixture.fail_cargo()?;

    assert_success(&fixture.run("post-edit")?);
    assert_eq!(line_count(&fixture.cargo_log)?, 1);
    assert_eq!(
        fs::read_to_string(&fixture.xtask_log)?,
        "agent-hook pre-bash\nagent-hook post-edit\n"
    );

    let pre_bash = fixture.run("pre-bash")?;
    assert!(!pre_bash.status.success());
    assert_eq!(line_count(&fixture.cargo_log)?, 2);
    assert!(!fixture.cache_dir().join("target").exists());
    Ok(())
}
