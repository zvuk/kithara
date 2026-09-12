//! Fixture server startup, readiness and owned process cleanup.

use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use tracing::warn;

use crate::{child, ci::process::Process};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Port {
    /// Apple simulator suites are serialized on the host, so a fixed port is
    /// free there and keeps the URL knowable in advance.
    Fixed(u16),
    /// An Android run shares its host with whatever else the executor is doing,
    /// where a chosen number is a collision waiting for the next job.
    Ephemeral,
}

impl Port {
    const fn requested(self) -> u16 {
        match self {
            Self::Fixed(port) => port,
            Self::Ephemeral => 0,
        }
    }
}

struct Consts;

impl Consts {
    /// The line the server prints once its listener is bound. Its origin is
    /// `STARTUP_RECORD` in `tests/crates/integration/src/test_server/native.rs`.
    const STARTUP_RECORD: &'static str = "test server listening on";
    const POLL: Duration = Duration::from_millis(200);
    const READY: Duration = Duration::from_secs(60);
}

pub(crate) struct TestServer {
    /// Absent when the process only recorded the request to start one.
    child: Option<Child>,
    drain: Option<JoinHandle<()>>,
    url: String,
}

impl TestServer {
    /// Returns once the server answers `/health`. `log` collects everything
    /// the child writes.
    pub(crate) fn start(
        process: &Process,
        port: Port,
        log: &Path,
        cancel: Option<&child::Cancel>,
    ) -> Result<Self> {
        child::check(cancel)?;
        let mut build = process.command("cargo");
        build.args([
            "build",
            "-p",
            "kithara-integration-tests",
            "--bin",
            "test_server",
        ]);
        child::isolate(&mut build);
        if let Some(mut build) = process.spawn(&mut build, "hermetic test server")? {
            let status = child::supervise(&mut build, cancel, None)?;
            if !status.success() {
                bail!("hermetic test server build failed: {status}");
            }
        }

        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let log_file = File::create(log).with_context(|| format!("creating {}", log.display()))?;

        let binary = process.target_dir().join("debug/test_server");
        let mut command = process.command(&binary);
        command
            .env("TEST_SERVER_PORT", port.requested().to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log_file));
        child::check(cancel)?;
        child::isolate(&mut command);
        let Some(mut child) = process.spawn(&mut command, "hermetic test server")? else {
            return Ok(Self {
                child: None,
                drain: None,
                url: recorded_url(port),
            });
        };

        let mut server = Self {
            child: None,
            drain: None,
            url: String::new(),
        };
        let stdout = child.stdout.take();
        server.child = Some(child);
        let started = (|| {
            let stdout = stdout.context("fixture server stdout was not captured")?;
            let (announced, drain) = drain_stdout(stdout, log)?;
            server.drain = Some(drain);
            server.url = server.await_bound_url(&announced, port, cancel)?;
            server.await_health(cancel)
        })();
        if let Err(error) = started {
            return Err(match server.shutdown() {
                Ok(()) => error.context("fixture server cleanup: ok"),
                Err(stop_error) => error.context(stop_error),
            });
        }

        Ok(server)
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// [`Drop`] does the same silently for a caller that leaves early.
    pub(crate) fn stop(mut self) -> Result<()> {
        self.shutdown()
    }

    /// A run that adopted a stranger's server would report another job's
    /// fixtures as its own.
    fn await_bound_url(
        &mut self,
        announced: &Receiver<String>,
        port: Port,
        cancel: Option<&child::Cancel>,
    ) -> Result<String> {
        let deadline = Instant::now() + Consts::READY;
        loop {
            child::check(cancel)?;
            match announced.recv_timeout(Consts::POLL) {
                Ok(line) => {
                    let bound = bound_port(&line)?;
                    if let Port::Fixed(expected) = port
                        && bound != expected
                    {
                        bail!(
                            "the hermetic test server was asked for port {expected} and bound {bound}"
                        );
                    }
                    return Ok(format!("http://127.0.0.1:{bound}"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("{}", self.exit_context("closed its output"))
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.exited()? {
                        bail!("the hermetic test server exited with {status} before it bound");
                    }
                    if Instant::now() >= deadline {
                        bail!(
                            "the hermetic test server did not report a bound address within {}s",
                            Consts::READY.as_secs()
                        );
                    }
                }
            }
        }
    }

    fn await_health(&mut self, cancel: Option<&child::Cancel>) -> Result<()> {
        let client = Client::builder()
            .timeout(Consts::POLL)
            .build()
            .context("building the readiness client")?;
        let endpoint = format!("{}/health", self.url);
        let deadline = Instant::now() + Consts::READY;
        loop {
            child::check(cancel)?;
            if let Some(status) = self.exited()? {
                bail!("the hermetic test server exited with {status} before it answered");
            }
            if let Ok(response) = client.get(&endpoint).send()
                && response.status().is_success()
                && response.text().is_ok_and(|body| body.trim() == "ok")
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "the hermetic test server did not answer {endpoint} within {}s",
                    Consts::READY.as_secs()
                );
            }
            thread::sleep(Consts::POLL);
        }
    }

    fn exited(&mut self) -> Result<Option<std::process::ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        child
            .try_wait()
            .context("checking on the hermetic test server")
    }

    fn exit_context(&mut self, what: &str) -> String {
        match self.exited() {
            Ok(Some(status)) => {
                format!("the hermetic test server {what} and exited with {status}")
            }
            Ok(None) => format!("the hermetic test server {what} while still running"),
            Err(error) => format!("the hermetic test server {what}: {error}"),
        }
    }

    fn shutdown(&mut self) -> Result<()> {
        let outcome = self.child.take().map_or(Ok(()), |mut child| {
            child::stop(&mut child, "the hermetic test server").map(drop)
        });
        if let Some(drain) = self.drain.take()
            && outcome.is_ok()
            && drain.join().is_err()
        {
            warn!("the hermetic test server log writer panicked");
        }
        outcome
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            warn!(%error, "the hermetic test server did not stop cleanly");
        }
    }
}

/// Reading continues past the address line: a full pipe stops the server
/// mid-run.
fn drain_stdout(
    stdout: std::process::ChildStdout,
    log: &Path,
) -> Result<(Receiver<String>, JoinHandle<()>)> {
    let mut file = OpenOptions::new()
        .append(true)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let (sender, receiver) = mpsc::channel();
    let drain = thread::spawn(move || {
        let mut announced = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = writeln!(file, "{line}");
            if !announced && line.contains(Consts::STARTUP_RECORD) {
                announced = sender.send(line).is_ok();
            }
        }
    });
    Ok((receiver, drain))
}

/// The port out of `test server listening on http://127.0.0.1:34809/`.
fn bound_port(line: &str) -> Result<u16> {
    let address = line
        .trim()
        .rsplit_once(Consts::STARTUP_RECORD)
        .map(|(_, address)| address.trim())
        .context("the hermetic test server announced no address")?;
    let authority = address
        .trim_end_matches('/')
        .strip_prefix("http://")
        .with_context(|| format!("`{address}` is not an HTTP address"))?;
    let (_, port) = authority
        .rsplit_once(':')
        .with_context(|| format!("`{address}` carries no port"))?;
    port.parse()
        .with_context(|| format!("`{address}` carries no numeric port"))
}

/// What a recorded run reports, where no child ever answers. A fixed port is
/// known without asking; an OS-assigned one is not, and the recording says so
/// rather than inventing a number a reader could mistake for a real one.
fn recorded_url(port: Port) -> String {
    match port {
        Port::Fixed(port) => format!("http://127.0.0.1:{port}"),
        Port::Ephemeral => "http://127.0.0.1:<assigned at run time>".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_port_reads_the_announced_address() {
        assert_eq!(
            bound_port("test server listening on http://127.0.0.1:34809/").unwrap(),
            34_809
        );
    }

    #[test]
    fn bound_port_reads_an_address_without_a_trailing_slash() {
        assert_eq!(
            bound_port("test server listening on http://127.0.0.1:3444").unwrap(),
            3_444
        );
    }

    #[test]
    fn bound_port_rejects_a_line_that_carries_no_address() {
        assert!(bound_port("test server listening on ").is_err());
    }

    #[test]
    fn bound_port_rejects_a_non_http_address() {
        assert!(bound_port("test server listening on unix:/tmp/socket").is_err());
    }

    #[test]
    fn recorded_fixed_port_keeps_the_url_a_lane_would_pass() {
        assert_eq!(recorded_url(Port::Fixed(3444)), "http://127.0.0.1:3444");
    }
}
