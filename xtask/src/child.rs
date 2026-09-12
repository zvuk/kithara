//! Owned process groups, cancellation, and deadlines for control commands.

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::{
    io::Read as _,
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use signal_hook::{
    SigId,
    consts::signal::{SIGINT, SIGTERM},
    flag, low_level,
};

struct Consts;

impl Consts {
    const POLL: Duration = Duration::from_millis(20);
    /// How long a child gets to leave after it is killed outright.
    const GRACE: Duration = Duration::from_secs(10);
}

/// Children of a run get their own process group, including commands they spawn.
#[cfg(unix)]
pub(crate) fn isolate(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn isolate(_command: &mut Command) {}

pub(crate) fn spawn(command: &mut Command) -> Result<Child> {
    isolate(command);
    command.spawn().context("spawning owned command")
}

/// Stop the owned group even when its leader has already exited.
#[cfg(unix)]
fn kill_owned(child: &mut Child) -> Result<()> {
    let pid = i32::try_from(child.id()).context("child process group id")?;
    match killpg(Pid::from_raw(pid), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(error).context("stopping owned process group"),
    }
}

#[cfg(not(unix))]
fn kill_owned(child: &mut Child) -> Result<()> {
    let mut taskkill = Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .spawn()
        .context("stopping owned process tree")?;
    let status = if let Some(status) = reap(&mut taskkill, Consts::GRACE, "taskkill")? {
        status
    } else {
        taskkill.kill().context("stopping timed-out taskkill")?;
        reap(&mut taskkill, Consts::GRACE, "taskkill")?.context("taskkill did not exit")?;
        bail!("taskkill exceeded its deadline");
    };
    if !status.success() && child.try_wait()?.is_none() {
        bail!("taskkill failed: {status}");
    }
    Ok(())
}

pub(crate) fn stop(child: &mut Child, what: &str) -> Result<ExitStatus> {
    kill_owned(child).with_context(|| format!("stopping {what}"))?;
    reap(child, Consts::GRACE, what)?.with_context(|| {
        format!(
            "{what} outlived the {}s stop grace",
            Consts::GRACE.as_secs()
        )
    })
}

pub(crate) fn check(cancel: Option<&Cancel>) -> Result<()> {
    let signal = cancel.map_or(0, |cancel| cancel.pending.load(Ordering::SeqCst));
    if signal != 0 {
        bail!("run cancelled by signal {signal}");
    }
    Ok(())
}

/// A deadline is required for control commands, while builds end on cancellation.
pub(crate) fn supervise(
    child: &mut Child,
    cancel: Option<&Cancel>,
    timeout: Option<Duration>,
) -> Result<ExitStatus> {
    let started = Instant::now();
    loop {
        let proceeding = check(cancel).and_then(|()| {
            if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                bail!("owned command exceeded its deadline");
            }
            Ok(())
        });
        if let Err(error) = proceeding {
            let stopped = stop(child, "cancelled or timed-out command");
            return Err(match stopped {
                Ok(_) => error,
                Err(stop_error) => error.context(stop_error),
            });
        }
        match child.try_wait().context("waiting on owned command") {
            Ok(Some(status)) => {
                kill_owned(child)?;
                return Ok(status);
            }
            Ok(None) => thread::sleep(Consts::POLL),
            Err(error) => {
                return Err(match stop(child, "failed command wait") {
                    Ok(_) => error,
                    Err(cleanup) => error.context(cleanup),
                });
            }
        }
    }
}

pub(crate) fn run(command: &mut Command, cancel: Option<&Cancel>) -> Result<ExitStatus> {
    check(cancel)?;
    supervise(&mut spawn(command)?, cancel, None)
}

/// Drain both pipes while the command runs, so a full pipe cannot prevent exit.
pub(crate) fn output(
    command: &mut Command,
    cancel: Option<&Cancel>,
    timeout: Duration,
) -> Result<Output> {
    check(cancel)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn(command)?;
    let stdout = child.stdout.take().context("captured stdout")?;
    let stderr = child.stderr.take().context("captured stderr")?;
    let read = |mut stream: Box<dyn std::io::Read + Send>| {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream.read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send(result);
        });
        receiver
    };
    let stdout = read(Box::new(stdout));
    let stderr = read(Box::new(stderr));
    let status = supervise(&mut child, cancel, Some(timeout))?;
    let stdout = stdout
        .recv_timeout(Consts::GRACE)
        .context("draining command stdout")??;
    let stderr = stderr
        .recv_timeout(Consts::GRACE)
        .context("draining command stderr")??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn reap(child: &mut Child, grace: Duration, what: &str) -> Result<Option<ExitStatus>> {
    let deadline = Instant::now() + grace;
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("waiting on {what}"))?
        {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Consts::POLL);
    }
}

/// Record SIGINT/SIGTERM until the run has released its resources.
/// Dropping registrations does not restore default signal dispositions.
pub(crate) struct Cancel {
    pending: Arc<AtomicUsize>,
    registrations: Vec<SigId>,
}

impl Cancel {
    pub(crate) fn install() -> Result<Self> {
        let pending = Arc::new(AtomicUsize::new(0));
        let mut registrations = Vec::new();
        for signal in [SIGINT, SIGTERM] {
            let value = usize::try_from(signal).context("signal number out of range")?;
            let registration = flag::register_usize(signal, Arc::clone(&pending), value)
                .with_context(|| format!("installing the handler for signal {signal}"))?;
            registrations.push(registration);
        }
        Ok(Self {
            pending,
            registrations,
        })
    }
}

impl Drop for Cancel {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            low_level::unregister(registration);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn shell(script: &str) -> Child {
        spawn(
            Command::new("sh")
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .expect("sh spawns")
    }

    #[test]
    fn supervised_command_returns_its_exit_status() {
        let mut child = shell("exit 7");
        assert_eq!(
            supervise(&mut child, None, Some(Duration::from_secs(5)))
                .unwrap()
                .code(),
            Some(7)
        );
    }

    #[test]
    fn an_early_leader_exit_releases_its_descendants() {
        let mut child = spawn(
            Command::new("sh")
                .args(["-c", "sleep 30 & echo ready; exit 7"])
                .stdout(Stdio::piped()),
        )
        .unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        std::io::BufRead::read_line(&mut stdout, &mut ready).unwrap();
        assert_eq!(ready, "ready\n");
        assert_eq!(supervise(&mut child, None, None).unwrap().code(), Some(7));
        let (closed, receive) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            closed.send(stdout.read_to_end(&mut bytes)).unwrap();
        });
        receive
            .recv_timeout(Duration::from_secs(2))
            .expect("descendant outlived the command")
            .unwrap();
    }

    #[test]
    fn control_command_deadline_stops_a_pipe_holding_descendant() {
        let start = Instant::now();
        let result = output(
            Command::new("sh").args(["-c", "sleep 30 & wait"]),
            None,
            Duration::from_millis(100),
        );
        assert!(result.unwrap_err().to_string().contains("deadline"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn cancellation_stops_the_child_group_and_preserves_an_unrelated_child() {
        let mut unrelated = shell("sleep 30");
        let cancel = Cancel::install().unwrap();
        let mut owned = spawn(
            Command::new("sh")
                .args(["-c", "sleep 30 & echo ready; wait"])
                .stdout(Stdio::piped()),
        )
        .unwrap();
        let mut stdout = std::io::BufReader::new(owned.stdout.take().unwrap());
        let mut ready = String::new();
        std::io::BufRead::read_line(&mut stdout, &mut ready).unwrap();
        assert_eq!(ready, "ready\n");
        for signal in [SIGINT, SIGTERM] {
            let expected = usize::try_from(signal).unwrap();
            low_level::raise(signal).unwrap();
            assert_eq!(cancel.pending.load(Ordering::SeqCst), expected);
        }
        let error = supervise(&mut owned, Some(&cancel), None).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(owned.try_wait().unwrap().is_some());
        let (closed, receive) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            closed.send(stdout.read_to_end(&mut bytes)).unwrap();
        });
        receive
            .recv_timeout(Duration::from_secs(2))
            .expect("descendant still holds stdout")
            .unwrap();
        assert!(unrelated.try_wait().unwrap().is_none());
        stop(&mut unrelated, "unrelated test process").unwrap();
    }
}
