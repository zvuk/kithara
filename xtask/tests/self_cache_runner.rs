#![cfg(unix)]

use std::{
    env, fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    bootstrap_artifact: PathBuf,
    bootstrap_cargo: PathBuf,
    cargo_log: PathBuf,
    fake_bin: PathBuf,
    git_log: PathBuf,
    justfile: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let fake_bin = temp.path().join("bin");
        let cargo_log = temp.path().join("cargo.log");
        let git_log = temp.path().join("git.log");
        let bootstrap_cargo = temp.path().join("bootstrap-cargo");
        let justfile = root.join("justfile");
        fs::create_dir_all(root.join(".config"))?;
        fs::create_dir_all(root.join(".git"))?;
        fs::create_dir_all(root.join("xtask/src"))?;
        fs::create_dir_all(&fake_bin)?;
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("resolve repository root")?;
        fs::copy(repository.join("justfile"), &justfile)?;
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"xtask\"]\n",
        )?;
        fs::write(
            root.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
        )?;
        fs::write(
            root.join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )?;
        fs::write(root.join("xtask/src/main.rs"), "fn main() {}\n")?;
        write_config(&root, false)?;
        write_executable(
            &fake_bin.join("cargo"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SELF_CACHE_CARGO_LOG"
printf 'target=%s\n' "${CARGO_TARGET_DIR-}" >> "$SELF_CACHE_CARGO_LOG"
exit 97
"#,
        )?;
        let bootstrap_artifact = root.join("target/xtask-self-cache/debug/xtask");
        fs::create_dir_all(
            bootstrap_artifact
                .parent()
                .context("bootstrap artifact has no parent")?,
        )?;
        fs::copy(env!("CARGO_BIN_EXE_xtask"), &bootstrap_artifact)?;
        let mut permissions = fs::metadata(&bootstrap_artifact)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bootstrap_artifact, permissions)?;
        write_executable(
            &bootstrap_cargo,
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$SELF_CACHE_BOOTSTRAP_ARTIFACT\"\n",
        )?;
        write_executable(
            &fake_bin.join("git"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SELF_CACHE_GIT_LOG"
exit 98
"#,
        )?;
        Ok(Self {
            _temp: temp,
            bootstrap_artifact,
            bootstrap_cargo,
            cargo_log,
            fake_bin,
            git_log,
            justfile,
            root,
        })
    }

    fn bootstrap(&self) -> Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["self-cache", "bootstrap"])
            .current_dir(&self.root)
            .env("CARGO", env!("CARGO"))
            .env("XTASK_SELF_CACHE_CARGO", &self.bootstrap_cargo)
            .env("SELF_CACHE_BOOTSTRAP_ARTIFACT", &self.bootstrap_artifact)
            .stdin(Stdio::null())
            .output()
            .context("bootstrap self-cache fixture")
    }

    fn active_generation(&self) -> Result<PathBuf> {
        let body = fs::read_to_string(self.root.join("xtask/.xtask-cache"))?;
        Ok(PathBuf::from(body.trim()))
    }

    fn active_binary(&self) -> Result<PathBuf> {
        Ok(self.active_generation()?.join("xtask"))
    }

    fn cached(&self, args: &[&str]) -> Result<Output> {
        self.cached_from(&self.root, args)
    }

    fn cached_with_input(&self, args: &[&str], input: &[u8]) -> Result<Output> {
        let mut command = Command::new(self.active_binary()?);
        command
            .args(args)
            .current_dir(&self.root)
            .env("PATH", self.fake_path()?)
            .env("SELF_CACHE_CARGO_LOG", &self.cargo_log)
            .env("SELF_CACHE_GIT_LOG", &self.git_log)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        child
            .stdin
            .take()
            .context("open cached xtask stdin")?
            .write_all(input)?;
        child.wait_with_output().context("run cached xtask")
    }

    fn cached_from(&self, root: &Path, args: &[&str]) -> Result<Output> {
        let fake_cargo = self.fake_bin.join("cargo");
        Command::new(self.active_binary()?)
            .args(args)
            .current_dir(root)
            .env("CARGO", env!("CARGO"))
            .env("XTASK_SELF_CACHE_CARGO", &fake_cargo)
            .env("PATH", self.fake_path()?)
            .env("SELF_CACHE_CARGO_LOG", &self.cargo_log)
            .env("SELF_CACHE_GIT_LOG", &self.git_log)
            .stdin(Stdio::null())
            .output()
            .context("run cached xtask")
    }

    fn transport(&self, root: &Path) -> Result<Output> {
        self.just(
            root,
            &["_xtask-cached", "strict", "self-cache", "probe"],
            None,
        )
    }

    fn just(&self, root: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Output> {
        let mut command = Command::new("just");
        command
            .arg("--justfile")
            .arg(&self.justfile)
            .arg("--working-directory")
            .arg(root)
            .args(args)
            .env("CARGO", env!("CARGO"))
            .env("PATH", self.fake_path()?)
            .env("SELF_CACHE_CARGO_LOG", &self.cargo_log)
            .env("SELF_CACHE_GIT_LOG", &self.git_log)
            .env("SELF_CACHE_TEST_XTASK", env!("CARGO_BIN_EXE_xtask"))
            .env("XTASK_SELF_CACHE_CARGO", &self.bootstrap_cargo)
            .env("SELF_CACHE_BOOTSTRAP_ARTIFACT", &self.bootstrap_artifact)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("start Just self-cache transport")?;
        if let Some(input) = input {
            child
                .stdin
                .take()
                .context("open Just transport stdin")?
                .write_all(input)?;
        }
        child
            .wait_with_output()
            .context("run Just self-cache transport")
    }

    fn install_fake_transport(&self) -> Result<()> {
        let generation = self.root.join(".git/xtask-cache/generation-fake");
        fs::create_dir_all(&generation)?;
        write_executable(
            &generation.join("xtask"),
            r#"#!/bin/sh
set -eu
case "${1-}" in
  self-cache)
    [ "${2-}" = probe ] || exit 2
    ;;
  record)
    shift
    for argument in "$@"; do
      printf 'arg=<%s>\n' "$argument"
    done
    IFS= read -r input || true
    printf 'stdin=<%s>\n' "$input"
    ;;
  exit-status)
    exit "$2"
    ;;
  terminate)
    kill -TERM "$$"
    exit 99
    ;;
  *)
    exit 3
    ;;
esac
"#,
        )?;
        fs::write(
            self.root.join("xtask/.xtask-cache"),
            format!("{}\n", generation.display()),
        )?;
        Ok(())
    }

    fn install_outer_bootstrap_cargo(&self) -> Result<()> {
        write_executable(
            &self.fake_bin.join("cargo"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SELF_CACHE_CARGO_LOG"
exec "$SELF_CACHE_TEST_XTASK" self-cache bootstrap
"#,
        )
    }

    fn fake_path(&self) -> Result<std::ffi::OsString> {
        let mut paths = vec![self.fake_bin.clone()];
        if let Some(path) = env::var_os("PATH") {
            paths.extend(env::split_paths(&path));
        }
        env::join_paths(paths).context("construct fixture PATH")
    }

    fn generation_count(&self) -> Result<usize> {
        let cache = self.root.join(".git/xtask-cache");
        Ok(fs::read_dir(cache)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("generation-"))
            })
            .count())
    }

    fn use_linked_git_dir(&self, git_dir: &Path) -> Result<()> {
        fs::remove_dir(self.root.join(".git"))?;
        fs::create_dir_all(git_dir)?;
        fs::write(
            self.root.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )?;
        Ok(())
    }

    fn assert_no_tool_process(&self) {
        assert!(!self.cargo_log.exists(), "self-cache invoked Cargo");
        assert!(!self.git_log.exists(), "self-cache invoked Git");
    }
}

#[test]
fn warm_probe_and_status_are_cargo_and_git_free() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    let binary = fixture.active_binary()?;

    let probe = fixture.cached(&["self-cache", "probe"])?;
    let status = fixture.cached(&["self-cache", "status"])?;

    assert_success(&probe);
    assert_success(&status);
    assert_eq!(probe.stdout, format!("{}\n", binary.display()).as_bytes());
    assert_eq!(status.stdout, b"current\n");
    fixture.assert_no_tool_process();
    Ok(())
}

#[test]
fn warm_public_just_runner_and_hook_are_cargo_and_git_free() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);

    let help = fixture.just(&fixture.root, &["xtask", "--help"], None)?;
    assert_success(&help);
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: xtask"));

    write_config(&fixture.root, true)?;
    let payload = br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;
    let direct = fixture.cached_with_input(&["agent-hook"], payload)?;
    assert_success(&direct);
    assert!(
        String::from_utf8_lossy(&direct.stdout).contains("\"permissionDecision\":\"deny\""),
        "unexpected direct hook output: stdout={} stderr={}",
        String::from_utf8_lossy(&direct.stdout),
        String::from_utf8_lossy(&direct.stderr)
    );
    let hook = fixture.just(&fixture.root, &["agent-hook"], Some(payload))?;
    assert_success(&hook);
    assert!(
        String::from_utf8_lossy(&hook.stdout).contains("\"permissionDecision\":\"deny\""),
        "unexpected hook output: stdout={} stderr={}",
        String::from_utf8_lossy(&hook.stdout),
        String::from_utf8_lossy(&hook.stderr)
    );
    fixture.assert_no_tool_process();
    Ok(())
}

#[test]
fn cached_just_transport_preserves_arguments_stdin_and_exit_codes() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.install_fake_transport()?;

    let output = fixture.just(
        &fixture.root,
        &[
            "_xtask-cached",
            "strict",
            "record",
            "",
            "two words",
            "*",
            "; echo injected",
            "'quoted'",
        ],
        Some(b"payload\n"),
    )?;
    assert_success(&output);
    assert_eq!(
        output.stdout,
        b"arg=<>\narg=<two words>\narg=<*>\narg=<; echo injected>\narg=<'quoted'>\nstdin=<payload>\n"
    );

    for code in [42, 126, 127] {
        let code = code.to_string();
        let output = fixture.just(
            &fixture.root,
            &["_xtask-cached", "strict", "exit-status", &code],
            None,
        )?;
        assert_eq!(output.status.code(), Some(code.parse()?));
    }
    for code in [126, 127] {
        let code = code.to_string();
        let output = fixture.just(
            &fixture.root,
            &["_xtask-cached", "optional", "exit-status", &code],
            None,
        )?;
        assert_eq!(output.status.code(), Some(code.parse()?));
    }
    let terminated = fixture.just(
        &fixture.root,
        &["_xtask-cached", "optional", "terminate"],
        None,
    )?;
    assert_eq!(terminated.status.code(), Some(128 + Signal::SIGTERM as i32));
    fixture.assert_no_tool_process();
    Ok(())
}

#[test]
fn optional_transport_fails_open_before_policy_starts() -> Result<()> {
    let fixture = Fixture::new()?;

    let missing = fixture.just(&fixture.root, &["agent-hook"], Some(b"ignored"))?;

    assert_success(&missing);
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("cached xtask transport is unavailable"),
        "unexpected optional output: stdout={} stderr={}",
        String::from_utf8_lossy(&missing.stdout),
        String::from_utf8_lossy(&missing.stderr)
    );
    fixture.assert_no_tool_process();
    Ok(())
}

#[test]
fn source_changes_stale_but_hook_routes_do_not() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    write_config(&fixture.root, true)?;

    let route_only = fixture.cached(&["self-cache", "status"])?;
    assert_success(&route_only);
    assert_eq!(route_only.stdout, b"current\n");

    fs::write(
        fixture.root.join("xtask/src/main.rs"),
        "fn main() { changed(); }\n",
    )?;
    let source = fixture.cached(&["self-cache", "status"])?;
    assert_success(&source);
    assert_eq!(source.stdout, b"stale\n");

    let payload = br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;
    let hook = fixture.just(&fixture.root, &["agent-hook"], Some(payload))?;
    assert_success(&hook);
    assert!(
        String::from_utf8_lossy(&hook.stdout).contains("\"permissionDecision\":\"deny\""),
        "unexpected stale hook output: stdout={} stderr={}",
        String::from_utf8_lossy(&hook.stdout),
        String::from_utf8_lossy(&hook.stderr)
    );
    fixture.assert_no_tool_process();
    Ok(())
}

#[test]
fn linked_worktrees_publish_distinct_caches() -> Result<()> {
    let common = tempfile::tempdir()?;
    let first = Fixture::new()?;
    let second = Fixture::new()?;
    let worktrees = common.path().join("common.git/worktrees");
    let first_git = worktrees.join("first");
    let second_git = worktrees.join("second");
    first.use_linked_git_dir(&first_git)?;
    second.use_linked_git_dir(&second_git)?;

    assert_success(&first.bootstrap()?);
    assert_success(&second.bootstrap()?);

    let first_generation = first.active_generation()?;
    let second_generation = second.active_generation()?;
    let first_cache = fs::canonicalize(first_git)?.join("xtask-cache");
    let second_cache = fs::canonicalize(second_git)?.join("xtask-cache");
    assert_eq!(first_generation.parent(), Some(first_cache.as_path()));
    assert_eq!(second_generation.parent(), Some(second_cache.as_path()));
    assert_ne!(first_generation, second_generation);
    assert_success(&first.transport(&first.root)?);
    assert_success(&second.transport(&second.root)?);
    first.assert_no_tool_process();
    second.assert_no_tool_process();
    Ok(())
}

#[test]
fn public_status_errors_do_not_start_cargo() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    fs::write(
        fixture.root.join(".config/xtask.toml"),
        "this is not valid TOML\n",
    )?;

    let output = fixture.just(&fixture.root, &["xtask", "--help"], None)?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("parse"),
        "unexpected status error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_no_tool_process();
    Ok(())
}

#[test]
fn concurrent_public_stale_refreshes_build_once() -> Result<()> {
    let fixture = Arc::new(Fixture::new()?);
    assert_success(&fixture.bootstrap()?);
    fs::write(
        fixture.root.join("xtask/src/main.rs"),
        "fn main() { changed(); }\n",
    )?;
    write_executable(
        &fixture.bootstrap_cargo,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SELF_CACHE_CARGO_LOG"
sleep 0.2
printf '%s\n' "$SELF_CACHE_BOOTSTRAP_ARTIFACT"
"#,
    )?;
    let barrier = Arc::new(Barrier::new(9));
    let handles = (0..8)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let fixture = Arc::clone(&fixture);
            thread::spawn(move || {
                barrier.wait();
                fixture.just(&fixture.root, &["xtask", "--help"], None)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        let output = handle
            .join()
            .map_err(|_| anyhow!("public xtask thread panicked"))??;
        assert_success(&output);
    }

    assert_eq!(fs::read_to_string(&fixture.cargo_log)?.lines().count(), 1);
    assert_eq!(fixture.generation_count()?, 2);
    assert!(!fixture.git_log.exists());
    Ok(())
}

#[test]
fn corrupt_and_cross_worktree_locators_are_rejected() -> Result<()> {
    let first = Fixture::new()?;
    let second = Fixture::new()?;
    assert_success(&first.bootstrap()?);
    assert_success(&second.bootstrap()?);
    fs::write(
        second.root.join("xtask/.xtask-cache"),
        format!("{}\n", first.active_generation()?.display()),
    )?;

    let cross_worktree = second.transport(&second.root)?;
    assert!(!cross_worktree.status.success());

    fs::write(second.root.join("xtask/.xtask-cache"), "relative\n")?;
    let corrupt = second.transport(&second.root)?;
    assert!(!corrupt.status.success());
    first.assert_no_tool_process();
    second.assert_no_tool_process();
    Ok(())
}

#[test]
fn corrupt_executable_is_repaired_by_public_just_runner() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    let before = fixture.active_generation()?;
    fs::write(fixture.active_binary()?, b"not an executable format\n")?;
    fixture.install_outer_bootstrap_cargo()?;

    let help = fixture.just(&fixture.root, &["xtask", "--help"], None)?;

    assert_success(&help);
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: xtask"));
    assert_ne!(fixture.active_generation()?, before);
    assert!(fixture.cargo_log.exists());
    Ok(())
}

#[test]
fn failed_refresh_preserves_locator_and_uses_canonical_cargo_args() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    let locator = fixture.root.join("xtask/.xtask-cache");
    let before = fs::read(&locator)?;

    let refresh = fixture.cached(&["self-cache", "refresh", "--force"])?;

    assert!(!refresh.status.success());
    assert_eq!(fs::read(locator)?, before);
    let cargo = fs::read_to_string(&fixture.cargo_log)?;
    let canonical_root = fs::canonicalize(&fixture.root)?;
    assert_eq!(
        cargo,
        format!(
            "run --locked --quiet --manifest-path {}/Cargo.toml -p xtask --bin xtask -- self-cache artifact\ntarget={}/target/xtask-self-cache\n",
            canonical_root.display(),
            canonical_root.display()
        )
    );
    assert!(!fixture.git_log.exists());
    Ok(())
}

#[test]
fn concurrent_cold_bootstraps_publish_one_generation() -> Result<()> {
    let fixture = Arc::new(Fixture::new()?);
    let barrier = Arc::new(Barrier::new(9));
    let handles = (0..8)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let fixture = Arc::clone(&fixture);
            thread::spawn(move || {
                barrier.wait();
                fixture.bootstrap()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        let output = handle
            .join()
            .map_err(|_| anyhow!("bootstrap thread panicked"))??;
        assert_success(&output);
    }

    assert_eq!(fixture.generation_count()?, 1);
    Ok(())
}

#[test]
fn killed_refresh_parent_does_not_leave_builder_descendants() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    let before = fs::read(fixture.root.join("xtask/.xtask-cache"))?;
    let cargo_pid = fixture.root.join("cargo.pid");
    let descendant_pid = fixture.root.join("descendant.pid");
    let worker_pid = fixture.root.join("worker.pid");
    write_executable(
        &fixture.fake_bin.join("cargo"),
        r#"#!/bin/sh
set -eu
trap 'exit 0' TERM
printf '%s\n' "$PPID" > "$SELF_CACHE_WORKER_PID"
printf '%s\n' "$$" > "$SELF_CACHE_CARGO_PID"
sh -c 'trap "" TERM; while :; do sleep 300; done' &
descendant=$!
printf '%s\n' "$descendant" > "$SELF_CACHE_DESCENDANT_PID"
wait "$descendant"
"#,
    )?;
    let fake_cargo = fixture.fake_bin.join("cargo");
    let mut command = Command::new(fixture.active_binary()?);
    command
        .args(["self-cache", "refresh", "--force"])
        .current_dir(&fixture.root)
        .env("CARGO", env!("CARGO"))
        .env("XTASK_SELF_CACHE_CARGO", &fake_cargo)
        .env("SELF_CACHE_CARGO_PID", &cargo_pid)
        .env("SELF_CACHE_DESCENDANT_PID", &descendant_pid)
        .env("SELF_CACHE_WORKER_PID", &worker_pid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut refresh = command.spawn()?;
    wait_for_file(&cargo_pid)?;
    wait_for_file(&descendant_pid)?;
    wait_for_file(&worker_pid)?;
    let cargo = read_pid(&cargo_pid)?;
    let descendant = read_pid(&descendant_pid)?;
    let worker = read_pid(&worker_pid)?;

    refresh.kill()?;
    refresh.wait()?;

    wait_for_exit(cargo)?;
    wait_for_exit(descendant)?;
    wait_for_exit(worker)?;
    assert_eq!(fs::read(fixture.root.join("xtask/.xtask-cache"))?, before);
    Ok(())
}

fn write_config(root: &Path, with_route: bool) -> Result<()> {
    let route = if with_route {
        r#"
[ext.agent_hook]
destructive_git_override_env = "OVERRIDE"

[[ext.agent_hook.routes]]
event = "pre-tool-use"
tool_kind = "shell"
handler = "command-guard"
"#
    } else {
        ""
    };
    fs::write(
        root.join(".config/xtask.toml"),
        format!(
            "[project]\nname = \"fixture\"\n\n[ext.xtask.cache]\nextra_inputs = []\nkeep_generations = 2\ngeneration_grace_secs = 3600\n{route}"
        ),
    )?;
    Ok(())
}

fn write_executable(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.is_file() {
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for {}", path.display()));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn read_pid(path: &Path) -> Result<i32> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .with_context(|| format!("parse process id from {}", path.display()))
}

fn wait_for_exit(process: i32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match kill(Pid::from_raw(process), None) {
            Err(Errno::ESRCH) => return Ok(()),
            Ok(()) | Err(Errno::EPERM) => {}
            Err(error) => return Err(error).context("probe supervised process"),
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("supervised process {process} did not exit"));
        }
        thread::sleep(Duration::from_millis(10));
    }
}
