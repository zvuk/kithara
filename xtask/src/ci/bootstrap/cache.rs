use std::{
    env,
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::Instant,
};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::{super::cache_report, consts};

/// Require the effective daemon, rather than merely a configured remote URL.
///
/// # Errors
/// Returns an error for malformed statistics or an unavailable, read-only or non-S3 daemon.
pub fn require_remote(stats: &str) -> io::Result<()> {
    let stats: Value = serde_json::from_str(stats)
        .map_err(|_| io::Error::other("invalid compiler-cache remote policy"))?;
    if !stats["remote"]
        .as_str()
        .is_some_and(|remote| remote.starts_with("s3://") && remote.len() > 5)
    {
        return Err(io::Error::other(
            "CI compilation requires the scoped RustFS S3 backend",
        ));
    }
    if stats["daemon_connected"] != true {
        return Err(io::Error::other("the compiler cache daemon is unavailable"));
    }
    if stats["daemon_remote_readonly"] != false {
        return Err(io::Error::other(
            "CI compilation requires a writable RustFS cache daemon",
        ));
    }
    Ok(())
}

/// Own a fresh bootstrap daemon while retaining the provisioned host layer.
pub struct CompilerCache {
    directory: TempDir,
    active: bool,
}

impl CompilerCache {
    /// Validate a fresh runtime before allowing the compilation child to start.
    ///
    /// # Errors
    /// Returns an error when host configuration, the daemon or its writable S3 remote is unavailable.
    pub fn enter() -> io::Result<Option<Self>> {
        if !is_ci() {
            return Ok(None);
        }
        require_host()?;
        let cache = Self {
            directory: tempfile::Builder::new()
                .prefix("kithara-compiler.")
                .tempdir()?,
            active: true,
        };
        cache.verify_remote()?;
        Ok(Some(cache))
    }

    /// Apply the scope's write policy and isolate this phase from other jobs.
    pub fn apply(&self, command: &mut Command) {
        compilation_environment(command, self.directory.path());
    }

    /// Retain the normal bounded report before stopping the daemon.
    pub fn record(&self, phase: &OsStr) {
        record(self.directory.path(), phase);
    }

    /// Drain queued uploads and report a failed stop to the compilation owner.
    ///
    /// # Errors
    /// Returns an error when the writable daemon cannot be established, drained or stopped.
    pub fn finish(mut self) -> io::Result<()> {
        // The idle policy can retire this daemon while the compilation child is quiet.
        let verified = self.verify_remote();
        let stopped = self.capture(&["daemon", "stop"]).map(|_| ());
        self.active = false;
        verified.and(stopped)
    }

    fn verify_remote(&self) -> io::Result<()> {
        require_remote(&super::statistics(
            &mut kache_command(self.directory.path(), &[]),
            self.directory.path(),
        )?)
    }

    fn capture(&self, args: &[&str]) -> io::Result<String> {
        let mut command = kache_command(self.directory.path(), args);
        output(&mut command).map_err(|error| {
            io::Error::new(error.kind(), format!("kache {}: {error}", args.join(" ")))
        })
    }
}

impl Drop for CompilerCache {
    fn drop(&mut self) {
        if self.active && self.capture(&["daemon", "stop"]).is_err() {
            eprintln!("bootstrap compiler-cache daemon did not stop");
        }
    }
}

pub(super) fn output(command: &mut Command) -> io::Result<String> {
    output_bounded(command, consts::OUTPUT_LIMIT)
}

fn output_bounded(command: &mut Command, limit: u64) -> io::Result<String> {
    let mut output = tempfile::tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::inherit());
    let mut child = command.spawn()?;
    let started = Instant::now();
    let status: ExitStatus = loop {
        let pending = match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => output.metadata().and_then(|metadata| {
                if metadata.len() > limit {
                    Err(io::Error::other("compiler-cache output exceeds its bound"))
                } else if started.elapsed() >= consts::COMMAND_TIMEOUT {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "compiler-cache command timed out",
                    ))
                } else {
                    Ok(())
                }
            }),
            Err(error) => Err(error),
        };
        if let Err(error) = pending {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        thread::sleep(consts::POLL_INTERVAL);
    };
    if !status.success() {
        return Err(io::Error::other(format!(
            "compiler-cache command failed ({status})"
        )));
    }
    if output.metadata()?.len() > limit {
        return Err(io::Error::other("compiler-cache output exceeds its bound"));
    }
    output.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    output.take(limit).read_to_string(&mut text)?;
    Ok(text)
}

fn configured(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn is_ci() -> bool {
    ["CI_JOB_ID", "GITHUB_RUN_ID"].into_iter().any(configured)
}

fn require_host() -> io::Result<()> {
    for name in ["KACHE_HOST_CONFIG", "SCCACHE_BUCKET", "SCCACHE_ENDPOINT"] {
        if !configured(name) {
            return Err(io::Error::other(format!("CI compilation requires {name}")));
        }
    }
    File::open(
        env::var_os("KACHE_HOST_CONFIG")
            .ok_or_else(|| io::Error::other("missing kache host layer"))?,
    )?;
    Ok(())
}

fn compilation_environment(command: &mut Command, runtime: &Path) {
    command
        .env("GITHUB_ACTIONS", "")
        .env("GITLAB_CI", "")
        .env("KACHE_RUNTIME_DIR", runtime)
        .env("KACHE_DAEMON_IDLE_TIMEOUT", consts::DAEMON_IDLE_TIMEOUT)
        .env("KACHE_PREFETCH_ENABLED", "false")
        .env_remove("KACHE_SOCKET_PATH")
        .env("RUSTC_WRAPPER", "kache");
}

fn kache_command(runtime: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("kache");
    command.args(args);
    compilation_environment(&mut command, runtime);
    command
}

fn record_observation(
    steps: &mut Vec<Value>,
    operation: impl FnOnce() -> io::Result<String>,
) -> io::Result<String> {
    let started = Instant::now();
    let result = operation();
    steps.push(
        json!({"elapsed_ms": started.elapsed().as_secs_f64() * 1000.0, "ok": result.is_ok()}),
    );
    result
}

fn record(runtime: &Path, phase: &OsStr) {
    let result = (|| {
        let mut command = Command::new(env::current_exe()?);
        command.args(["ci", "cache", "record"]).arg(phase);
        compilation_environment(&mut command, runtime);
        output(&mut command)
    })();
    if result.is_err() {
        eprintln!("compiler-cache evidence could not be recorded");
    }
}

/// Retain bootstrap observations without requiring a cached CLI generation.
///
/// # Errors
/// Returns an error for a missing runtime or failed evidence publication.
pub fn record_evidence(phase: &OsStr) -> io::Result<()> {
    let runtime = env::var_os("KACHE_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other("missing bootstrap cache runtime"))?;
    let runtime = Path::new(&runtime);
    let phase = phase
        .to_str()
        .ok_or_else(|| io::Error::other("invalid cache evidence phase"))?;
    let limit = u64::try_from(cache_report::INPUT_LIMIT).map_err(io::Error::other)?;
    let mut steps = Vec::new();
    let primary = record_observation(&mut steps, || {
        super::statistics(&mut kache_command(runtime, &[]), runtime)
    });
    let full = record_observation(&mut steps, || {
        output_bounded(
            &mut kache_command(runtime, &["stats", "--full", "--json"]),
            limit,
        )
    });
    let explain = record_observation(&mut steps, || {
        output_bounded(&mut kache_command(runtime, &["explain", "--json"]), limit)
    });
    let fallback = record_observation(&mut steps, || {
        let mut command = Command::new("sccache");
        command.arg("--show-stats");
        compilation_environment(&mut command, runtime);
        output_bounded(&mut command, limit)
    });
    let revision = record_observation(&mut steps, || {
        let mut command = Command::new("git");
        command.args(["rev-parse", "HEAD"]);
        output_bounded(&mut command, 64)
    });
    let mut report = cache_report::observations(
        cache_report::parse(primary.as_deref().ok()),
        cache_report::parse(full.as_deref().ok()),
        cache_report::parse(explain.as_deref().ok()),
        fallback.as_deref().ok(),
    );
    report["revision"] = json!(revision.as_deref().ok());
    report["steps"] = json!({"step_count": steps.len(), "steps": steps});
    report["key_history"] = cache_report::history(runtime).unwrap_or(Value::Null);
    report["transfer_history"] = cache_report::transfer_history(runtime).unwrap_or(Value::Null);
    cache_report::write(
        &env::current_dir()?,
        phase,
        |name| env::var_os(name),
        report,
    )
}

pub(super) fn finish_bootstrap(runtime: &Path) -> io::Result<()> {
    let owned = env::temp_dir();
    let named = runtime
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix("kithara-bootstrap."))
        .is_some_and(|suffix| {
            suffix.len() >= 6 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    let parent = runtime
        .parent()
        .ok_or_else(|| io::Error::other("bootstrap runtime has no parent"))?;
    if !named || runtime.is_symlink() || parent.canonicalize()? != owned.canonicalize()? {
        return Err(io::Error::other(
            "bootstrap runtime is outside its owned temporary directory",
        ));
    }
    if !runtime.exists() {
        return Ok(());
    }
    if !runtime.is_dir() {
        return Err(io::Error::other("bootstrap runtime is not a directory"));
    }
    let in_ci = is_ci();
    if !in_ci && fs::read_dir(runtime)?.next().is_none() {
        return fs::remove_dir(runtime);
    }
    let verified = (|| {
        if in_ci {
            require_host()?;
        }
        let stats = super::statistics(&mut kache_command(runtime, &[]), runtime)?;
        if in_ci {
            require_remote(&stats)?;
            record(runtime, OsStr::new("bootstrap-driver"));
        }
        Ok(())
    })();
    let stopped = output(&mut kache_command(runtime, &["daemon", "stop"]))
        .and_then(|_| fs::remove_dir_all(runtime));
    verified.and(stopped)
}
