use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
#[cfg(unix)]
use std::{
    io::Read,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    process::{Child, ExitStatus},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Instant,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill, killpg},
    unistd::{Pid, getppid},
};
#[cfg(unix)]
use signal_hook::{
    SigId,
    consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM},
    flag, low_level,
};

use super::{
    layout, lease,
    manifest::{CacheManifest, Freshness},
    publish,
};
use crate::config::{KitharaExt, XtaskCacheConfig};

struct Consts;

impl Consts {
    const BUILD_OUTPUT_LIMIT: usize = 4096;
    #[cfg(unix)]
    const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(20);
    #[cfg(unix)]
    const CHILD_TERMINATION_GRACE: Duration = Duration::from_secs(2);
}

#[derive(Debug, Args)]
pub(crate) struct SelfCacheArgs {
    #[command(subcommand)]
    command: SelfCacheCommand,
}

#[derive(Debug, Subcommand)]
enum SelfCacheCommand {
    Probe,
    Status,
    Refresh {
        #[arg(long)]
        force: bool,
    },
    Bootstrap {
        #[arg(long)]
        force: bool,
    },
    #[command(hide = true)]
    BuildWorker {
        #[arg(long)]
        parent: u32,
        #[arg(long)]
        release: bool,
    },
    #[command(hide = true)]
    Artifact,
}

pub(crate) fn run(args: &SelfCacheArgs) -> Result<()> {
    let root = layout::root()?;
    match &args.command {
        SelfCacheCommand::Probe => probe(),
        SelfCacheCommand::Status => status(&root),
        SelfCacheCommand::Refresh { force } => refresh(&root, *force),
        SelfCacheCommand::Bootstrap { force } => bootstrap(&root, *force),
        SelfCacheCommand::BuildWorker { parent, release } => build_worker(&root, *parent, *release),
        SelfCacheCommand::Artifact => artifact(&root),
    }
}

fn probe() -> Result<()> {
    let generation = layout::current()?.context("xtask is not running from a cache generation")?;
    println!("{}", generation.binary.display());
    Ok(())
}

fn status(root: &Path) -> Result<()> {
    let generation = layout::current()?.context("xtask is not running from a cache generation")?;
    let config = KitharaExt::load(root)?;
    let config = config.xtask_cache()?;
    let freshness = generation.manifest.freshness(root, config)?;
    if let Err(error) = lease::cleanup(
        root,
        &generation.path,
        config.keep_generations,
        Duration::from_secs(config.generation_grace_secs),
    ) {
        eprintln!("warning: self-cache cleanup failed: {error:#}");
    }
    match freshness {
        Freshness::Current => println!("current"),
        Freshness::Stale => println!("stale"),
    }
    Ok(())
}

fn refresh(root: &Path, force: bool) -> Result<()> {
    let config = KitharaExt::load(root)?;
    let config = config.xtask_cache()?;
    refresh_with(root, config, force, || rebuild(root, config))
}

fn refresh_with<F>(root: &Path, config: &XtaskCacheConfig, force: bool, build: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    let observed = layout::locator_snapshot(root)?;
    let _lock = lease::refresh(root)?;
    if force {
        if layout::locator_snapshot(root)? != observed && active_is_current(root, config) {
            return Ok(());
        }
    } else if active_is_current(root, config) {
        return Ok(());
    }
    build()?;
    ensure!(
        active_is_current(root, config),
        "xtask self-cache refresh did not publish a current generation"
    );
    Ok(())
}

fn bootstrap(root: &Path, force: bool) -> Result<()> {
    let config = KitharaExt::load(root)?;
    let config = config.xtask_cache()?;
    let observed = layout::locator_snapshot(root)?;
    let _lock = lease::refresh(root)?;
    let another_bootstrap_published = layout::locator_snapshot(root)? != observed;
    if (!force || another_bootstrap_published)
        && active_is_current(root, config)
        && active_is_runnable(root)
    {
        return Ok(());
    }
    rebuild(root, config)?;
    ensure!(
        active_is_current(root, config) && active_is_runnable(root),
        "xtask self-cache bootstrap did not publish a runnable current generation"
    );
    Ok(())
}

fn active_is_current(root: &Path, config: &XtaskCacheConfig) -> bool {
    let Ok(generation) = layout::active(root) else {
        return false;
    };
    matches!(
        generation.manifest.freshness(root, config),
        Ok(Freshness::Current)
    )
}

fn active_is_runnable(root: &Path) -> bool {
    let Ok(generation) = layout::active(root) else {
        return false;
    };
    Command::new(generation.binary)
        .args(["self-cache", "probe"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn rebuild(root: &Path, config: &XtaskCacheConfig) -> Result<()> {
    let expected = CacheManifest::discover(root, config)?;
    let artifact = build(root, expected.is_release())?;
    let actual = CacheManifest::discover(root, config)?;
    publish_unchanged(root, &artifact, &expected, &actual).map(|_| ())
}

fn publish_unchanged(
    root: &Path,
    artifact: &Path,
    expected: &CacheManifest,
    actual: &CacheManifest,
) -> Result<PathBuf> {
    ensure!(
        actual.source_stamp == expected.source_stamp,
        "xtask self-cache inputs changed while Cargo was building; refusing to publish"
    );
    publish::from_source(root, artifact, actual)
}

fn build(root: &Path, release: bool) -> Result<PathBuf> {
    let executable = fs::canonicalize(env::current_exe().context("resolve current xtask")?)
        .context("resolve current xtask executable")?;
    let mut command = Command::new(executable);
    command
        .args(["self-cache", "build-worker", "--parent"])
        .arg(std::process::id().to_string());
    if release {
        command.arg("--release");
    }
    let output = command
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("launch supervised Cargo worker for xtask self-cache refresh")?;
    ensure!(
        output.status.success(),
        "xtask self-cache build worker failed: {}",
        output.status
    );
    parse_artifact(root, &output.stdout)
}

#[cfg(unix)]
fn build_worker(root: &Path, parent: u32, release: bool) -> Result<()> {
    ensure!(parent > 1, "invalid self-cache build parent process");
    ensure!(
        parent_id()? == parent,
        "self-cache build parent changed before Cargo started"
    );
    let signals = BuildSignals::install()?;
    let cargo = env::var_os("XTASK_SELF_CACHE_CARGO")
        .or_else(|| env::var_os("CARGO"))
        .unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["run", "--locked", "--quiet", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .args(["-p", "xtask", "--bin", "xtask"]);
    if release {
        command.arg("--release");
    }
    command
        .args(["--", "self-cache", "artifact"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", layout::target_dir(root))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .context("launch Cargo for xtask self-cache refresh")?;
    let stdout = child
        .stdout
        .take()
        .context("capture xtask self-cache build output")?;
    let reader = thread::spawn(move || read_bounded_output(stdout, Consts::BUILD_OUTPUT_LIMIT));
    let status = supervise(&mut child, parent, &signals);
    let output = reader
        .join()
        .map_err(|_| anyhow::anyhow!("xtask self-cache output reader panicked"))?;
    let status = status?;
    let output = output?;
    ensure!(
        status.success(),
        "xtask self-cache refresh build failed: {status}"
    );
    let artifact = parse_artifact(root, &output)?;
    println!("{}", artifact.display());
    Ok(())
}

#[cfg(not(unix))]
fn build_worker(_root: &Path, _parent: u32, _release: bool) -> Result<()> {
    bail!("xtask self-cache build supervision requires a Unix host")
}

fn artifact(root: &Path) -> Result<()> {
    let executable = fs::canonicalize(env::current_exe().context("resolve built xtask")?)
        .context("resolve built xtask executable")?;
    let target = fs::canonicalize(layout::target_dir(root)).context("resolve xtask target dir")?;
    ensure!(
        executable.starts_with(&target),
        "built xtask is outside its private target directory: {}",
        executable.display()
    );
    println!("{}", executable.display());
    Ok(())
}

fn parse_artifact(root: &Path, output: &[u8]) -> Result<PathBuf> {
    ensure!(
        output.len() <= Consts::BUILD_OUTPUT_LIMIT,
        "xtask self-cache build output exceeds {} bytes",
        Consts::BUILD_OUTPUT_LIMIT
    );
    let body = std::str::from_utf8(output).context("xtask self-cache build output is not UTF-8")?;
    let value = body
        .strip_suffix('\n')
        .filter(|value| !value.is_empty() && !value.contains('\n'))
        .context("xtask self-cache build output must contain one artifact path")?;
    let artifact =
        fs::canonicalize(value).with_context(|| format!("resolve built xtask artifact {value}"))?;
    let target = fs::canonicalize(layout::target_dir(root)).context("resolve xtask target dir")?;
    ensure!(
        artifact.starts_with(&target),
        "built xtask artifact is outside its private target directory: {}",
        artifact.display()
    );
    let metadata = fs::symlink_metadata(&artifact)
        .with_context(|| format!("read built xtask artifact {}", artifact.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "built xtask artifact is not a regular file"
    );
    #[cfg(unix)]
    ensure!(
        metadata.permissions().mode() & 0o111 != 0,
        "built xtask artifact is not executable"
    );
    Ok(artifact)
}

#[cfg(unix)]
fn read_bounded_output(mut reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let take = u64::try_from(limit)?
        .checked_add(1)
        .context("self-cache build output limit overflow")?;
    let mut output = Vec::with_capacity(limit);
    reader
        .by_ref()
        .take(take)
        .read_to_end(&mut output)
        .context("read xtask self-cache build output")?;
    std::io::copy(&mut reader, &mut std::io::sink())
        .context("drain oversized xtask self-cache build output")?;
    ensure!(
        output.len() <= limit,
        "xtask self-cache build output exceeds {limit} bytes"
    );
    Ok(output)
}

#[cfg(unix)]
fn supervise(child: &mut Child, parent: u32, signals: &BuildSignals) -> Result<ExitStatus> {
    loop {
        let signal = signals.pending.load(Ordering::SeqCst);
        if signal != 0 {
            let signal = Signal::try_from(i32::try_from(signal)?)
                .context("decode xtask self-cache cancellation signal")?;
            terminate(child, signal)?;
            bail!("xtask self-cache build cancelled by {signal:?}");
        }
        if parent_id()? != parent {
            terminate(child, Signal::SIGTERM)?;
            bail!("xtask self-cache build parent exited");
        }
        if let Some(status) = child
            .try_wait()
            .context("wait for xtask self-cache Cargo process")?
        {
            return Ok(status);
        }
        thread::sleep(Consts::CHILD_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn terminate(child: &mut Child, signal: Signal) -> Result<()> {
    let process = child.id();
    signal_group(process, signal)?;
    let deadline = Instant::now() + Consts::CHILD_TERMINATION_GRACE;
    let mut leader_reaped = false;
    loop {
        if !leader_reaped
            && child
                .try_wait()
                .context("reap cancelled xtask self-cache Cargo process")?
                .is_some()
        {
            leader_reaped = true;
        }
        if !process_group_exists(process)? {
            if !leader_reaped {
                child
                    .wait()
                    .context("reap cancelled xtask self-cache Cargo process")?;
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            signal_group(process, Signal::SIGKILL)?;
            if !leader_reaped {
                child
                    .wait()
                    .context("reap killed xtask self-cache Cargo process")?;
            }
            wait_for_group_exit(process)?;
            return Ok(());
        }
        thread::sleep(Consts::CHILD_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn wait_for_group_exit(process: u32) -> Result<()> {
    let deadline = Instant::now() + Consts::CHILD_TERMINATION_GRACE;
    while process_group_exists(process)? {
        if Instant::now() >= deadline {
            bail!("killed xtask self-cache Cargo process group did not exit");
        }
        thread::sleep(Consts::CHILD_POLL_INTERVAL);
    }
    Ok(())
}

#[cfg(unix)]
fn process_group_exists(process: u32) -> Result<bool> {
    let process = i32::try_from(process).context("convert Cargo process group id")?;
    let group = process
        .checked_neg()
        .context("negate Cargo process group id")?;
    match kill(Pid::from_raw(group), None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(error).context("probe xtask self-cache Cargo process group"),
    }
}

#[cfg(unix)]
fn signal_group(process: u32, signal: Signal) -> Result<()> {
    let process = i32::try_from(process).context("convert Cargo process id")?;
    match killpg(Pid::from_raw(process), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(error).context("signal xtask self-cache Cargo process group"),
    }
}

#[cfg(unix)]
fn parent_id() -> Result<u32> {
    u32::try_from(getppid().as_raw()).context("convert self-cache build parent process id")
}

#[cfg(unix)]
struct BuildSignals {
    pending: Arc<AtomicUsize>,
    registrations: Vec<SigId>,
}

#[cfg(unix)]
impl BuildSignals {
    fn install() -> Result<Self> {
        let pending = Arc::new(AtomicUsize::new(0));
        let mut signals = Self {
            pending,
            registrations: Vec::new(),
        };
        for signal in [SIGHUP, SIGINT, SIGQUIT, SIGTERM] {
            let registration = flag::register_usize(
                signal,
                Arc::clone(&signals.pending),
                usize::try_from(signal)?,
            )
            .with_context(|| format!("register self-cache build signal {signal}"))?;
            signals.registrations.push(registration);
        }
        Ok(signals)
    }
}

#[cfg(unix)]
impl Drop for BuildSignals {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            low_level::unregister(registration);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Arc, Barrier,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use anyhow::{Result, anyhow};

    use super::{publish_unchanged, refresh_with};
    use crate::{
        config::XtaskCacheConfig,
        self_cache::{manifest::CacheManifest, publish},
    };

    fn fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf, XtaskCacheConfig)> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let source = root.join("source");
        fs::create_dir_all(root.join(".git"))?;
        fs::create_dir_all(root.join(".config"))?;
        fs::create_dir_all(root.join("xtask/src"))?;
        fs::write(root.join("justfile"), b"")?;
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"xtask\"]\n",
        )?;
        fs::write(root.join("Cargo.lock"), "version = 4\n")?;
        fs::write(
            root.join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
        )?;
        fs::write(root.join("xtask/src/main.rs"), "fn main() {}\n")?;
        fs::write(
            root.join(".config/xtask.toml"),
            "[ext.xtask.cache]\nextra_inputs = []\nkeep_generations = 2\ngeneration_grace_secs = 3600\n",
        )?;
        fs::write(&source, b"binary")?;
        let config = XtaskCacheConfig {
            extra_inputs: Vec::new(),
            keep_generations: 2,
            generation_grace_secs: 3600,
        };
        Ok((temp, root, source, config))
    }

    #[test]
    fn concurrent_stale_refreshes_publish_once() -> Result<()> {
        let (_temp, root, source, config) = fixture()?;
        let initial = CacheManifest::from_roots(&root, &config, vec![PathBuf::from("xtask")])?;
        publish::from_source(&root, &source, &initial)?;
        fs::write(root.join("xtask/src/main.rs"), "fn main() { changed(); }\n")?;

        let barrier = Arc::new(Barrier::new(5));
        let builds = Arc::new(AtomicUsize::new(0));
        let handles = (0..4)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let builds = Arc::clone(&builds);
                let config = config.clone();
                let root = root.clone();
                let source = source.clone();
                thread::spawn(move || {
                    barrier.wait();
                    refresh_with(&root, &config, false, || {
                        builds.fetch_add(1, Ordering::SeqCst);
                        let manifest = CacheManifest::from_roots(
                            &root,
                            &config,
                            vec![PathBuf::from("xtask")],
                        )?;
                        publish::from_source(&root, &source, &manifest).map(|_| ())
                    })
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow!("refresh thread panicked"))??;
        }

        assert_eq!(builds.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn source_mutation_during_build_is_not_published() -> Result<()> {
        let (_temp, root, source, config) = fixture()?;
        let expected = CacheManifest::from_roots(&root, &config, vec![PathBuf::from("xtask")])?;
        fs::write(root.join("xtask/src/main.rs"), "fn main() { changed(); }\n")?;
        let actual = CacheManifest::from_roots(&root, &config, vec![PathBuf::from("xtask")])?;

        let error = publish_unchanged(&root, &source, &expected, &actual)
            .expect_err("changed sources must reject the built artifact");

        assert!(format!("{error:#}").contains("inputs changed while Cargo was building"));
        assert!(!root.join("xtask/.xtask-cache").exists());
        Ok(())
    }
}
