#![cfg(unix)]

use std::{
    env, fs,
    io::{self, BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Barrier, mpsc},
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, anyhow};
use kithara_devtools::lock::FileLock;
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
    /// The Cargo home this fixture hands to every command it spawns.
    ///
    /// Discovering the xtask source closure runs `cargo metadata`, which
    /// takes the package cache lock of its Cargo home even for a workspace
    /// without dependencies. A CI host shares one Cargo home between its
    /// runners, so the inherited one would make this fixture wait for
    /// whatever another job is downloading.
    cargo_home: PathBuf,
    cargo_log: PathBuf,
    fake_bin: PathBuf,
    git_log: PathBuf,
    justfile: PathBuf,
    root: PathBuf,
    /// The build directory this fixture hands to every command it spawns.
    ///
    /// `_xtask-bootstrap` exports `CARGO_TARGET_DIR` for the self-cache build
    /// and `layout::target_dir` reads it back. The commands spawned here stand
    /// in for that recipe, so they name this directory instead of inheriting
    /// the one the surrounding lane is building into.
    target: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let fake_bin = temp.path().join("bin");
        let cargo_log = temp.path().join("cargo.log");
        let git_log = temp.path().join("git.log");
        let bootstrap_cargo = temp.path().join("bootstrap-cargo");
        let cargo_home = temp.path().join("cargo-home");
        let justfile = root.join("justfile");
        fs::create_dir_all(root.join(".config"))?;
        fs::create_dir_all(root.join(".config/ci"))?;
        fs::create_dir_all(root.join(".git/xtask-cache"))?;
        fs::create_dir_all(root.join("xtask/src"))?;
        fs::create_dir_all(&fake_bin)?;
        fs::create_dir_all(&cargo_home)?;
        fs::write(temp.path().join("kache-host.toml"), "[cache]\n")?;
        write_executable(
            &fake_bin.join("kache"),
            r#"#!/bin/sh
case "$1 $2" in
  'stats --json') printf '%s' '{"remote":"s3://fixture/kache","daemon_connected":true,"daemon_remote_readonly":false}' ;;
  'daemon stop') ;;
  *) exec "$@" ;;
esac
"#,
        )?;
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("resolve repository root")?;
        fs::copy(repository.join("justfile"), &justfile)?;
        fs::copy(
            repository.join(".config/ci/compiler-cache.sh"),
            root.join(".config/ci/compiler-cache.sh"),
        )?;
        fs::copy(
            repository.join("xtask/bootstrap_lease.rs"),
            root.join("xtask/bootstrap_lease.rs"),
        )?;
        let module_source = repository.join(".config/just");
        let module_target = root.join(".config/just");
        fs::create_dir_all(&module_target)?;
        for entry in fs::read_dir(&module_source)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), module_target.join(entry.file_name()))?;
            }
        }
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
        let target = fs::canonicalize(&root)
            .context("resolve fixture root")?
            .join("target/xtask-self-cache");
        let bootstrap_artifact = target.join("debug/xtask");
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
            cargo_home,
            cargo_log,
            fake_bin,
            git_log,
            justfile,
            root,
            target,
        })
    }

    fn bootstrap_command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["self-cache", "bootstrap"])
            .current_dir(&self.root)
            .env("CI_JOB_ID", "")
            .env("GITHUB_RUN_ID", "")
            .env("PATH", self.fake_path().expect("fixture PATH"))
            .env("CARGO_TARGET_DIR", &self.target)
            .env("CARGO", env!("CARGO"))
            .env("CARGO_HOME", &self.cargo_home)
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env(
                "KACHE_HOST_CONFIG",
                self._temp.path().join("kache-host.toml"),
            )
            .env("SCCACHE_BUCKET", "fixture")
            .env("SCCACHE_ENDPOINT", "http://cache:9000")
            .env("XTASK_SELF_CACHE_CARGO", &self.bootstrap_cargo)
            .env("SELF_CACHE_BOOTSTRAP_ARTIFACT", &self.bootstrap_artifact)
            .stdin(Stdio::null());
        command
    }

    fn bootstrap(&self) -> Result<Output> {
        self.bootstrap_command()
            .output()
            .context("bootstrap self-cache fixture")
    }

    fn active_generation(&self) -> Result<PathBuf> {
        let body = fs::read_to_string(locator(&self.root)?)?;
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
            .env("CI_JOB_ID", "")
            .env("GITHUB_RUN_ID", "")
            .env("CARGO_TARGET_DIR", &self.target)
            .env("CARGO_HOME", &self.cargo_home)
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
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
        self.cached_in(root, args, Some(&self.target))
    }

    fn cached_in(&self, root: &Path, args: &[&str], target: Option<&Path>) -> Result<Output> {
        self.cached_command(root, args, target)?
            .output()
            .context("run cached xtask")
    }

    fn cached_command(&self, root: &Path, args: &[&str], target: Option<&Path>) -> Result<Command> {
        let fake_cargo = self.fake_bin.join("cargo");
        let mut command = Command::new(self.active_binary()?);
        command
            .args(args)
            .current_dir(root)
            .env("CI_JOB_ID", "")
            .env("GITHUB_RUN_ID", "")
            .env("CARGO", env!("CARGO"))
            .env("CARGO_HOME", &self.cargo_home)
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env("XTASK_SELF_CACHE_CARGO", &fake_cargo)
            .env("PATH", self.fake_path()?)
            .env("SELF_CACHE_CARGO_LOG", &self.cargo_log)
            .env("SELF_CACHE_GIT_LOG", &self.git_log)
            .stdin(Stdio::null());
        match target {
            Some(target) => command.env("CARGO_TARGET_DIR", target),
            None => command.env_remove("CARGO_TARGET_DIR"),
        };
        Ok(command)
    }

    fn transport(&self, root: &Path) -> Result<Output> {
        self.just(
            root,
            &["_xtask-cached", "strict", "self-cache", "probe"],
            None,
        )
    }

    fn just(&self, root: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Output> {
        let mut command = self.just_command(root, args)?;
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("start Just self-cache transport")?;
        if let Some(input) = input {
            let mut stdin = child.stdin.take().context("open Just transport stdin")?;
            let written = stdin.write_all(input);
            drop(stdin);
            accept_a_refused_payload(written)?;
        }
        child
            .wait_with_output()
            .context("run Just self-cache transport")
    }

    fn just_command(&self, root: &Path, args: &[&str]) -> Result<Command> {
        let mut command = Command::new("just");
        command
            .arg("--justfile")
            .arg(&self.justfile)
            .arg("--working-directory")
            .arg(root)
            .args(args)
            .env("CI_JOB_ID", "")
            .env("GITHUB_RUN_ID", "")
            .env_remove("KITHARA_XTASK_TARGET")
            .env(
                "KACHE_HOST_CONFIG",
                self._temp.path().join("kache-host.toml"),
            )
            .env("SCCACHE_BUCKET", "fixture")
            .env("SCCACHE_ENDPOINT", "http://cache:9000")
            .env("CARGO_TARGET_DIR", &self.target)
            .env("CARGO", env!("CARGO"))
            .env("CARGO_HOME", &self.cargo_home)
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env("PATH", self.fake_path()?)
            .env("SELF_CACHE_CARGO_LOG", &self.cargo_log)
            .env("SELF_CACHE_GIT_LOG", &self.git_log)
            .env("SELF_CACHE_TEST_XTASK", env!("CARGO_BIN_EXE_xtask"))
            .env("XTASK_SELF_CACHE_CARGO", &self.bootstrap_cargo)
            .env("SELF_CACHE_BOOTSTRAP_ARTIFACT", &self.bootstrap_artifact);
        Ok(command)
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
    case "${2-}" in
      probe) ;;
      status) printf 'current\n' ;;
      refresh) printf 'target=%s\n' "${CARGO_TARGET_DIR-}" >> "$SELF_CACHE_CARGO_LOG" ;;
      *) exit 2 ;;
    esac
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
  lease-check)
    : > "$SELF_CACHE_READY"
    while [ ! -e "$SELF_CACHE_RELEASE" ]; do sleep 0.01; done
    ;;
  *)
    exit 3
    ;;
esac
"#,
        )?;
        fs::write(locator(&self.root)?, format!("{}\n", generation.display()))?;
        Ok(())
    }

    fn install_outer_bootstrap_cargo(&self) -> Result<()> {
        write_executable(
            &self.fake_bin.join("cargo"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SELF_CACHE_CARGO_LOG"
while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do shift; done
[ "$#" -gt 0 ] || { printf 'fixture cargo received no run arguments\n' >&2; exit 2; }
shift
exec "$SELF_CACHE_TEST_XTASK" "$@"
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
        fs::remove_dir_all(self.root.join(".git"))?;
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
fn fixture_metadata_does_not_inherit_compiler_wrappers() -> Result<()> {
    const CHILD: &str = "SELF_CACHE_FIXTURE_WRAPPER_CHILD";
    if env::var_os(CHILD).is_some() {
        let fixture = Fixture::new()?;
        assert_success(&fixture.bootstrap()?);
        let refresh = fixture.cached(&["self-cache", "refresh", "--force"])?;
        assert!(!refresh.status.success());
        assert_eq!(
            fs::read_to_string(&fixture.cargo_log)?,
            cargo_build_log(&fs::canonicalize(&fixture.root)?, &fixture.target)
        );
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let wrapper = temp.path().join("compiler-wrapper");
    let log = temp.path().join("wrapper.log");
    write_executable(
        &wrapper,
        "#!/bin/sh\nprintf 'unexpected inherited compiler wrapper\\n' > \"$SELF_CACHE_WRAPPER_LOG\"\nexit 91\n",
    )?;
    for variable in ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"] {
        let output = Command::new(env::current_exe()?)
            .args([
                "--exact",
                stringify!(fixture_metadata_does_not_inherit_compiler_wrappers),
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("SELF_CACHE_WRAPPER_LOG", &log)
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env(variable, &wrapper)
            .output()?;
        assert_success(&output);
        assert!(!log.exists(), "fixture invoked inherited {variable}");
    }
    Ok(())
}

#[test]
fn source_discovery_keeps_compiler_wrappers_for_builds_only() -> Result<()> {
    for variable in ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"] {
        let fixture = Fixture::new()?;
        let wrapper = fixture.fake_bin.join("compiler-wrapper");
        let log = fixture.root.join("metadata-wrapper.log");
        let build_log = fixture.root.join("build-wrappers.log");
        write_executable(
            &wrapper,
            "#!/bin/sh\nprintf 'metadata invoked compiler wrapper\n' > \"$SELF_CACHE_WRAPPER_LOG\"\nexit 91\n",
        )?;
        write_executable(
            &fixture.bootstrap_cargo,
            r#"#!/bin/sh
set -eu
printf '%s\n' "${RUSTC_WRAPPER-}" "${RUSTC_WORKSPACE_WRAPPER-}" > "$SELF_CACHE_BUILD_WRAPPERS"
printf '%s\n' "$SELF_CACHE_BOOTSTRAP_ARTIFACT"
"#,
        )?;
        let output = fixture
            .bootstrap_command()
            .env(variable, &wrapper)
            .env("SELF_CACHE_WRAPPER_LOG", &log)
            .env("SELF_CACHE_BUILD_WRAPPERS", &build_log)
            .output()?;
        assert_success(&output);
        assert!(!log.exists(), "source discovery invoked {variable}");
        let expected = if variable == "RUSTC_WRAPPER" {
            format!("{}\n\n", wrapper.display())
        } else {
            format!("\n{}\n", wrapper.display())
        };
        assert_eq!(fs::read_to_string(build_log)?, expected);
    }
    Ok(())
}

/// A command whose stdout is its answer keeps it clean while it waits: the
/// wait is announced on stderr.
#[test]
fn a_waiting_command_announces_on_stderr_and_keeps_stdout_for_its_answer() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.active_generation()?.join("lease.lock"))?;
    let cleanup = FileLock::try_exclusive(lease)
        .ok()
        .context("the generation lease is free before the command starts")?;
    let mut child = fixture
        .cached_command(
            &fixture.root,
            &["self-cache", "status"],
            Some(&fixture.target),
        )?
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let lines = stream_lines(&mut child)?;

    let (stream, waiting) = lines_until(&lines, "waiting for self-cache generation")?;
    assert_eq!(stream, Stream::Stderr, "{waiting}");
    drop(cleanup);
    let rest = lines.iter().collect::<Vec<_>>();
    assert!(child.wait()?.success());

    let answer = rest
        .iter()
        .filter(|(stream, _)| *stream == Stream::Stdout)
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>();
    assert_eq!(answer, ["current"]);
    assert!(
        rest.iter().any(|(stream, line)| *stream == Stream::Stderr
            && line.contains("took self-cache generation")),
        "{rest:?}"
    );
    Ok(())
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

    let help = fixture.just(&fixture.root, &["tooling", "xtask", "--help"], None)?;
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
    let hook = fixture.just(&fixture.root, &["_agent-hook"], Some(payload))?;
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

/// On CI the executor names where xtask builds, and a job holds that
/// directory for as long as xtask runs: it sits in the runner's build root,
/// where an eviction would take it from under the build. Taking it marks it
/// used now, the date an eviction orders by. The lane's build directory is
/// not the wrapper's business: Cargo is told an alias there, and a lease taken
/// through it would make a directory where the alias belongs.
#[test]
fn ci_public_just_runner_holds_the_xtask_build_before_xtask() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.install_fake_transport()?;
    let builds = fixture._temp.path().join("builds");
    let xtask = builds.join("xtask");
    let alias = builds.join("build");
    let lease_path = xtask.join(".kithara-job-lease");
    fs::create_dir_all(&xtask)?;
    let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    fs::File::create(&lease_path)?.set_modified(long_ago)?;
    let ready = fixture._temp.path().join("ready");
    let release = fixture._temp.path().join("release");
    let mut command = fixture.just_command(&fixture.root, &["_xtask", "lease-check"])?;
    command
        .env("CI", "true")
        .env("CI_JOB_ID", "lease-test")
        .env("KITHARA_XTASK_TARGET", &xtask)
        .env("CARGO_TARGET_DIR", &alias)
        .env("SELF_CACHE_READY", &ready)
        .env("SELF_CACHE_RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    wait_for_file(&ready, &mut child)?;
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lease_path)?;
    let heartbeat = xtask.join(".kithara-job-heartbeat");

    assert!(FileLock::try_exclusive(lease).is_err());
    assert!(heartbeat.is_file());
    assert!(
        fs::metadata(&lease_path)?.modified()? > long_ago,
        "taking the lease marks the xtask build used"
    );
    assert!(
        !alias.exists(),
        "the wrapper made a directory where the alias belongs"
    );

    fs::write(release, [])?;
    assert_success(&child.wait_with_output()?);
    assert!(!heartbeat.exists());
    Ok(())
}

/// The host's cache cleanup holds a build's lease exclusively while it
/// decides whether to evict. A job arriving then says so instead of sitting
/// silent, and starts its command only once it holds the lease.
#[test]
fn ci_public_just_runner_announces_a_wait_for_the_build_target_lease() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.install_fake_transport()?;
    let ready = fixture._temp.path().join("ready");
    let release = fixture._temp.path().join("release");
    let xtask = fixture._temp.path().join("builds/xtask");
    fs::create_dir_all(&xtask)?;
    let cleanup = FileLock::try_exclusive(fs::File::create(xtask.join(".kithara-job-lease"))?)
        .ok()
        .context("the build target lease is free before the job starts")?;
    let mut command = fixture.just_command(&fixture.root, &["_xtask", "lease-check"])?;
    command
        .env("CI", "true")
        .env("CI_JOB_ID", "lease-test")
        .env("KITHARA_XTASK_TARGET", &xtask)
        .env("SELF_CACHE_READY", &ready)
        .env("SELF_CACHE_RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let lines = stream_lines(&mut child)?;

    let (stream, waiting) = lines_until(&lines, "waiting for the CI build target lease")?;
    assert_eq!(stream, Stream::Stderr, "{waiting}");
    assert!(
        !ready.exists(),
        "the command ran before the lease was taken"
    );
    drop(cleanup);
    let (stream, took) = lines_until(&lines, "took the CI build target lease")?;
    assert_eq!(stream, Stream::Stderr, "{took}");
    wait_for_file(&ready, &mut child)?;
    fs::write(&release, [])?;
    assert!(child.wait()?.success());
    Ok(())
}

/// An eviction moves a build away before it lets go of the lease, so the lock
/// a waiting job then gets guards a directory that is gone. The job holds the
/// directory standing at the path instead.
#[test]
fn ci_public_just_runner_holds_the_xtask_build_an_eviction_moved_away() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.install_fake_transport()?;
    let ready = fixture._temp.path().join("ready");
    let release = fixture._temp.path().join("release");
    let builds = fixture._temp.path().join("builds");
    let xtask = builds.join("xtask");
    fs::create_dir_all(&xtask)?;
    let eviction = FileLock::try_exclusive(fs::File::create(xtask.join(".kithara-job-lease"))?)
        .ok()
        .context("the build target lease is free before the job starts")?;
    let mut command = fixture.just_command(&fixture.root, &["_xtask", "lease-check"])?;
    command
        .env("CI", "true")
        .env("CI_JOB_ID", "lease-test")
        .env("KITHARA_XTASK_TARGET", &xtask)
        .env("SELF_CACHE_READY", &ready)
        .env("SELF_CACHE_RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let lines = stream_lines(&mut child)?;
    lines_until(&lines, "waiting for the CI build target lease")?;

    fs::rename(&xtask, builds.join(".evicting-xtask"))?;
    drop(eviction);
    wait_for_file(&ready, &mut child)?;
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(xtask.join(".kithara-job-lease"))?;

    assert!(
        FileLock::try_exclusive(lease).is_err(),
        "the job holds the directory standing at the path"
    );
    fs::write(&release, [])?;
    assert!(child.wait()?.success());
    Ok(())
}

#[test]
fn ci_bootstrap_builds_where_the_executor_names() -> Result<()> {
    let fixture = Fixture::new()?;
    let xtask = fixture._temp.path().join("builds/xtask");
    let output = fixture
        .just_command(&fixture.root, &["_xtask-bootstrap", "--force"])?
        .env("KITHARA_XTASK_TARGET", &xtask)
        .output()?;

    assert_eq!(output.status.code(), Some(97));
    assert!(
        fs::read_to_string(&fixture.cargo_log)?.contains(&format!("target={}\n", xtask.display()))
    );
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
fn drive_rooted_windows_transport_uses_the_executable_suffix() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.install_fake_transport()?;
    write_executable(
        &fixture.fake_bin.join("uname"),
        "#!/bin/sh\nset -eu\nprintf 'MINGW64_NT-10.0\\n'\n",
    )?;
    let original = fixture.root.join(".git/xtask-cache/generation-fake");
    let generation = fixture.root.join(r"C:\cache\generation-fake");
    fs::rename(original, &generation)?;
    fs::rename(generation.join("xtask"), generation.join("xtask.exe"))?;
    fs::write(locator(&fixture.root)?, "C:\\cache\\generation-fake\n")?;

    let output = fixture.transport(&fixture.root)?;

    assert_success(&output);
    fixture.assert_no_tool_process();
    Ok(())
}

#[test]
fn optional_transport_fails_open_before_policy_starts() -> Result<()> {
    let fixture = Fixture::new()?;

    let missing = fixture.just(&fixture.root, &["_agent-hook"], Some(b"ignored"))?;

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

/// A generation compiled against an older config schema fails on the first
/// thing every hook route needs, before any route is read. The hook is
/// non-blocking, so without a guard the failure is noise on every tool call
/// rather than a stop; the transport it takes has a branch for exactly this,
/// and the guard has to reach it.
#[test]
fn the_optional_transport_refuses_a_config_it_cannot_read() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    fs::write(
        fixture.root.join(".config/xtask.toml"),
        "this is not valid TOML\n",
    )?;

    let hook = fixture.just(&fixture.root, &["_agent-hook"], Some(b"ignored"))?;

    assert_success(&hook);
    assert!(
        String::from_utf8_lossy(&hook.stderr).contains("cached xtask transport is unavailable"),
        "unexpected hook output: stdout={} stderr={}",
        String::from_utf8_lossy(&hook.stdout),
        String::from_utf8_lossy(&hook.stderr)
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
    let hook = fixture.just(&fixture.root, &["_agent-hook"], Some(payload))?;
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
fn foreign_schema_drift_does_not_wedge_the_cached_transport() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    let config = fixture.root.join(".config/xtask.toml");
    let drifted = format!(
        "{}\n[ext.apple]\nfield_from_a_later_schema = true\n",
        fs::read_to_string(&config)?
    );
    fs::write(&config, drifted)?;

    let status = fixture.cached(&["self-cache", "status"])?;
    assert_success(&status);
    assert_eq!(status.stdout, b"current\n");

    let warm = fixture.just(&fixture.root, &["tooling", "xtask", "--help"], None)?;
    assert_success(&warm);
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

    let output = fixture.just(&fixture.root, &["tooling", "xtask", "--help"], None)?;

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
fn legacy_status_errors_are_rechecked_by_the_refresh_owner() -> Result<()> {
    for valid_config in [true, false] {
        let fixture = Fixture::new()?;
        assert_success(&fixture.bootstrap()?);
        let before = fixture.active_generation()?;
        write_executable(
            &fixture.active_binary()?,
            r#"#!/bin/sh
set -eu
case "$1 $2" in
  'self-cache probe') exit 0 ;;
  'self-cache status') printf 'legacy cache input was removed\n' >&2; exit 1 ;;
  'self-cache refresh') exec "$SELF_CACHE_TEST_XTASK" "$@" ;;
  *) exit 2 ;;
esac
"#,
        )?;
        fs::write(
            fixture.root.join("xtask/src/main.rs"),
            "fn main() { changed(); }\n",
        )?;
        write_executable(
            &fixture.bootstrap_cargo,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SELF_CACHE_CARGO_LOG"
printf '%s\n' "$SELF_CACHE_BOOTSTRAP_ARTIFACT"
"#,
        )?;
        if !valid_config {
            fs::write(fixture.root.join(".config/xtask.toml"), "invalid TOML\n")?;
        }

        let output = fixture.just(&fixture.root, &["tooling", "xtask", "--help"], None)?;

        if valid_config {
            assert_success(&output);
            assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: xtask"));
            assert_ne!(fixture.active_generation()?, before);
            assert_eq!(fs::read_to_string(&fixture.cargo_log)?.lines().count(), 1);
            let status = fixture.cached(&["self-cache", "status"])?;
            assert_success(&status);
            assert_eq!(status.stdout, b"current\n");
        } else {
            assert!(!output.status.success());
            assert_eq!(fixture.active_generation()?, before);
            fixture.assert_no_tool_process();
        }
    }
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
                fixture.just(&fixture.root, &["tooling", "xtask", "--help"], None)
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
        locator(&second.root)?,
        format!("{}\n", first.active_generation()?.display()),
    )?;

    let cross_worktree = second.transport(&second.root)?;
    assert!(!cross_worktree.status.success());

    fs::write(locator(&second.root)?, "relative\n")?;
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

    let help = fixture.just(&fixture.root, &["tooling", "xtask", "--help"], None)?;

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
    let locator = locator(&fixture.root)?;
    let before = fs::read(&locator)?;

    let refresh = fixture.cached(&["self-cache", "refresh", "--force"])?;

    assert!(!refresh.status.success());
    assert_eq!(fs::read(locator)?, before);
    let canonical_root = fs::canonicalize(&fixture.root)?;
    assert_eq!(
        fs::read_to_string(&fixture.cargo_log)?,
        cargo_build_log(
            &canonical_root,
            &canonical_root.join("target/xtask-self-cache")
        )
    );
    assert!(!fixture.git_log.exists());
    Ok(())
}

#[test]
fn a_named_cargo_target_directory_carries_the_refresh_build() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    let elsewhere = fixture.root.join("../shared-target");
    fs::create_dir_all(&elsewhere)?;
    let elsewhere = fs::canonicalize(&elsewhere)?;

    let refresh = fixture.cached_in(
        &fixture.root,
        &["self-cache", "refresh", "--force"],
        Some(&elsewhere),
    )?;

    assert!(!refresh.status.success());
    assert_eq!(
        fs::read_to_string(&fixture.cargo_log)?,
        cargo_build_log(&fs::canonicalize(&fixture.root)?, &elsewhere)
    );
    Ok(())
}

#[test]
fn an_unnamed_cargo_target_directory_builds_inside_the_checkout() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);

    let refresh = fixture.cached_in(&fixture.root, &["self-cache", "refresh", "--force"], None)?;

    assert!(!refresh.status.success());
    let root = fs::canonicalize(&fixture.root)?;
    assert_eq!(
        fs::read_to_string(&fixture.cargo_log)?,
        cargo_build_log(&root, &root.join("target/xtask-self-cache"))
    );
    Ok(())
}

/// A CI job finds the checkout's `target` linked to the build the last lane
/// made, so a stale self-cache refreshed into `target` would leave xtask's
/// units in a lane's directory. The refresh builds where the bootstrap does.
#[test]
fn a_stale_ci_self_cache_refreshes_where_the_bootstrap_builds() -> Result<()> {
    writeln!(io::stderr(), "self-cache-ci-refresh fixture:start")?;
    let fixture = Fixture::new()?;
    writeln!(
        io::stderr(),
        "self-cache-ci-refresh bootstrap:start root={} target={}",
        fixture.root.display(),
        fixture.target.display()
    )?;
    let mut bootstrap = fixture
        .bootstrap_command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start self-cache fixture bootstrap")?;
    writeln!(
        io::stderr(),
        "self-cache-ci-refresh bootstrap:spawned pid={}",
        bootstrap.id()
    )?;
    for (stream, line) in stream_lines(&mut bootstrap)? {
        writeln!(
            io::stderr(),
            "self-cache-ci-refresh bootstrap {stream:?}: {line}"
        )?;
    }
    let bootstrap = bootstrap.wait_with_output()?;
    writeln!(
        io::stderr(),
        "self-cache-ci-refresh bootstrap:finished status={}",
        bootstrap.status
    )?;
    assert_success(&bootstrap);
    fs::write(
        fixture.root.join("xtask/src/main.rs"),
        "fn main() { changed(); }\n",
    )?;
    let xtask = fixture._temp.path().join("builds/xtask");

    writeln!(
        io::stderr(),
        "self-cache-ci-refresh ready:start target={}",
        xtask.display()
    )?;
    let mut ready = fixture
        .just_command(&fixture.root, &["_xtask-ready"])?
        .env_remove("CARGO_TARGET_DIR")
        .env("KITHARA_XTASK_TARGET", &xtask)
        .env("XTASK_SELF_CACHE_CARGO", fixture.fake_bin.join("cargo"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start stale self-cache ready check")?;
    writeln!(
        io::stderr(),
        "self-cache-ci-refresh ready:spawned pid={}",
        ready.id()
    )?;
    for (stream, line) in stream_lines(&mut ready)? {
        writeln!(
            io::stderr(),
            "self-cache-ci-refresh ready {stream:?}: {line}"
        )?;
    }
    let output = ready.wait_with_output()?;
    writeln!(
        io::stderr(),
        "self-cache-ci-refresh ready:finished status={}",
        output.status
    )?;

    assert!(
        !output.status.success(),
        "the fixture cargo refuses every build"
    );
    assert_eq!(
        fs::read_to_string(&fixture.cargo_log)?,
        cargo_build_log(&fs::canonicalize(&fixture.root)?, &xtask)
    );
    Ok(())
}

#[test]
fn ci_bootstrap_lease_passes_the_selected_compiler_to_the_cache() -> Result<()> {
    let fixture = Fixture::new()?;
    let selected = fixture.fake_bin.join("selected compiler");
    let log = fixture._temp.path().join("compiler.log");
    write_executable(&fixture.fake_bin.join("rustc"), "#!/bin/sh\nexit 96\n")?;
    write_executable(
        &fixture.fake_bin.join("rustup"),
        "#!/bin/sh\nset -eu\ntest \"$*\" = 'which rustc'\nprintf '%s\\n' \"$SELECTED_RUSTC\"\n",
    )?;
    write_executable(
        &selected,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$0\" > \"$SELF_CACHE_COMPILER_LOG\"\nwhile test \"$1\" != -o; do shift; done\nprintf '#!/bin/sh\\nexit 0\\n' > \"$2\"\nchmod +x \"$2\"\n",
    )?;
    let output = fixture
        .just_command(&fixture.root, &["_xtask", "--help"])?
        .env("CI_JOB_ID", "42")
        .env("KITHARA_XTASK_TARGET", &fixture.target)
        .env("SELECTED_RUSTC", &selected)
        .env("SELF_CACHE_COMPILER_LOG", &log)
        .env("TMPDIR", fixture._temp.path())
        .output()?;
    assert_success(&output);
    assert_eq!(
        fs::read_to_string(log)?,
        format!("{}\n", selected.display())
    );
    Ok(())
}

#[test]
fn a_cached_ci_worker_refreshes_a_checkout_without_its_wrapper_script() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_success(&fixture.bootstrap()?);
    fs::remove_file(fixture.root.join(".config/ci/compiler-cache.sh"))?;
    fs::write(
        fixture.root.join("xtask/src/main.rs"),
        "fn main() { changed(); }\n",
    )?;
    let output = fixture
        .just_command(&fixture.root, &["_xtask-ready"])?
        .env("GITHUB_RUN_ID", "checkout-transition")
        .env("GITHUB_ACTIONS", "true")
        .env_remove("KACHE_RUNTIME_DIR")
        .env("XTASK_SELF_CACHE_CARGO", fixture.fake_bin.join("cargo"))
        .output()?;

    assert!(!output.status.success(), "the fixture Cargo refuses builds");
    assert_eq!(
        fs::read_to_string(&fixture.cargo_log)?,
        cargo_build_log(&fs::canonicalize(&fixture.root)?, &fixture.target)
    );
    Ok(())
}

/// Outside CI the refresh builds where `_xtask-bootstrap` does, not in a
/// build directory the caller happened to export.
#[test]
fn a_local_refresh_builds_where_the_local_bootstrap_does() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.install_fake_transport()?;
    let lane = fixture._temp.path().join("lane");

    let output = fixture
        .just_command(&fixture.root, &["_xtask-refresh"])?
        .env("CARGO_TARGET_DIR", &lane)
        .output()?;

    assert_success(&output);
    let log = fs::read_to_string(&fixture.cargo_log)?;
    let target = PathBuf::from(
        log.trim()
            .strip_prefix("target=")
            .context("the refresh logged its target")?,
    );
    assert!(target.ends_with("target/xtask-self-cache"), "{log}");
    assert_eq!(
        fs::canonicalize(
            target
                .parent()
                .and_then(Path::parent)
                .context("the target has a workspace root")?
        )?,
        fs::canonicalize(&fixture.root)?
    );
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
    let before = fs::read(locator(&fixture.root)?)?;
    let cargo_pid = fixture.root.join("cargo.pid");
    let descendant_pid = fixture.root.join("descendant.pid");
    let worker_pid = fixture.root.join("worker.pid");
    write_executable(
        &fixture.fake_bin.join("cargo"),
        r#"#!/bin/sh
set -eu
trap 'exit 0' TERM
publish_pid() {
    printf '%s\n' "$1" > "$2.pending"
    mv "$2.pending" "$2"
}
publish_pid "$PPID" "$SELF_CACHE_WORKER_PID"
publish_pid "$$" "$SELF_CACHE_CARGO_PID"
sh -c 'trap "" TERM; while :; do sleep 300; done' &
descendant=$!
publish_pid "$descendant" "$SELF_CACHE_DESCENDANT_PID"
wait "$descendant"
"#,
    )?;
    let fake_cargo = fixture.fake_bin.join("cargo");
    let mut command = Command::new(fixture.active_binary()?);
    command
        .args(["self-cache", "refresh", "--force"])
        .current_dir(&fixture.root)
        .env("CARGO_TARGET_DIR", &fixture.target)
        .env("CARGO", env!("CARGO"))
        .env("CARGO_HOME", &fixture.cargo_home)
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("XTASK_SELF_CACHE_CARGO", &fake_cargo)
        .env("SELF_CACHE_CARGO_PID", &cargo_pid)
        .env("SELF_CACHE_DESCENDANT_PID", &descendant_pid)
        .env("SELF_CACHE_WORKER_PID", &worker_pid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut refresh = command.spawn()?;
    wait_for_file(&cargo_pid, &mut refresh)?;
    wait_for_file(&descendant_pid, &mut refresh)?;
    wait_for_file(&worker_pid, &mut refresh)?;
    let cargo = read_pid(&cargo_pid)?;
    let descendant = read_pid(&descendant_pid)?;
    let worker = read_pid(&worker_pid)?;

    refresh.kill()?;
    refresh.wait()?;

    wait_for_exit(cargo)?;
    wait_for_exit(descendant)?;
    wait_for_exit(worker)?;
    assert_eq!(fs::read(locator(&fixture.root)?)?, before);
    Ok(())
}

/// The Git directory a checkout uses, spelled as a directory or, for a linked
/// worktree, named by the `.git` file.
fn git_dir(root: &Path) -> Result<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Ok(dot_git);
    }
    let body = fs::read_to_string(&dot_git)?;
    let named = body
        .trim()
        .strip_prefix("gitdir: ")
        .context("fixture .git file has no gitdir")?;
    Ok(PathBuf::from(named))
}

/// Where the pointer to the active generation lives, beside the generations
/// themselves rather than in the working tree the runner cleans.
fn locator(root: &Path) -> Result<PathBuf> {
    Ok(git_dir(root)?.join("xtask-cache/active"))
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

fn cargo_build_log(root: &Path, target: &Path) -> String {
    format!(
        "run --locked --manifest-path {}/Cargo.toml -p xtask --bin xtask -- self-cache artifact\ntarget={}\n",
        root.display(),
        target.display()
    )
}

fn write_executable(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

// A transport that is unavailable refuses before it reads the payload, so the
// read end can already be closed when the write lands. Who wins that race
// decides nothing: the exit status and the stderr each caller asserts on still
// have to say why. Failing the write instead would make the test's verdict
// depend on scheduling.
fn accept_a_refused_payload(written: io::Result<()>) -> Result<()> {
    match written {
        Err(error) if error.kind() != ErrorKind::BrokenPipe => {
            Err(error).context("write the transport payload")
        }
        _ => Ok(()),
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

/// Every line the child prints, tagged with the stream it came on, until
/// both streams close.
fn stream_lines(child: &mut Child) -> Result<mpsc::Receiver<(Stream, String)>> {
    let (sender, receiver) = mpsc::channel();
    forward(
        child.stdout.take().context("child stdout is not piped")?,
        Stream::Stdout,
        sender.clone(),
    );
    forward(
        child.stderr.take().context("child stderr is not piped")?,
        Stream::Stderr,
        sender,
    );
    Ok(receiver)
}

fn forward<R: Read + Send + 'static>(
    reader: R,
    stream: Stream,
    sender: mpsc::Sender<(Stream, String)>,
) {
    thread::spawn(move || {
        for line in BufReader::new(reader)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if sender.send((stream, line)).is_err() {
                break;
            }
        }
    });
}

/// The first line containing `needle`. The condition is the child's output,
/// not the clock: an error means the child closed both streams first.
fn lines_until(lines: &mpsc::Receiver<(Stream, String)>, needle: &str) -> Result<(Stream, String)> {
    lines
        .iter()
        .find(|(_, line)| line.contains(needle))
        .with_context(|| format!("the child closed its output without printing `{needle}`"))
}

/// Waits for `path` while `process` is still alive.
///
/// The condition is the process, not the clock. Reaching this point means
/// spawning `just`, which compiles the lease helper with a real `rustc` before
/// it can write anything, so a wall-clock deadline measures how busy the
/// machine is rather than whether the runner works: on a loaded host these
/// waits timed out at exactly their deadline while the helper was still
/// compiling. A process that exits without producing the file is the actual
/// defect, and it is reported here with the status it exited on.
fn wait_for_file(path: &Path, process: &mut Child) -> Result<()> {
    while !path.is_file() {
        if let Some(status) = process.try_wait()? {
            return Err(anyhow!(
                "the supervised process exited ({status}) without producing {}",
                path.display()
            ));
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
