//! The fixture HTTP server the browser tests fetch media from.
//!
//! `test_server` is built by `kithara-integration-tests` next to this binary.
//! It is reused when one already answers on the port, else started for the run.

use std::{
    env,
    process::{Child, Command, Stdio},
};

use anyhow::{Context, Result, bail};
use kithara_platform::time::{Duration, Instant, sleep};

const DEFAULT_PORT: u16 = 3444;
const DEFAULT_STARTUP_SECS: u64 = 30;

/// A `test_server` this run started; dropping it stops the server.
pub(crate) struct FixtureServer(Option<Child>);

impl Drop for FixtureServer {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn setting(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

async fn ready(url: &str) -> bool {
    reqwest::get(url).await.is_ok()
}

pub(crate) async fn ensure() -> Result<FixtureServer> {
    let port = u16::try_from(setting("TEST_SERVER_PORT", DEFAULT_PORT.into()))
        .context("TEST_SERVER_PORT")?;
    let startup = Duration::from_secs(setting(
        "TEST_SERVER_STARTUP_TIMEOUT_SECS",
        DEFAULT_STARTUP_SECS,
    ));
    let health = format!("http://127.0.0.1:{port}/health");
    if ready(&health).await {
        eprintln!("Test server already running on port {port}");
        return Ok(FixtureServer(None));
    }
    let binary = env::current_exe()?
        .parent()
        .context("the runner binary has no directory")?
        .join("test_server")
        .with_extension(env::consts::EXE_EXTENSION);
    let child = Command::new(&binary)
        .env("TEST_SERVER_PORT", port.to_string())
        .stdout(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", binary.display()))?;
    let mut server = FixtureServer(Some(child));
    let deadline = Instant::now() + startup;
    while !ready(&health).await {
        if let Some(child) = &mut server.0
            && let Some(status) = child.try_wait()?
        {
            bail!("test_server exited before becoming ready: {status}");
        }
        if Instant::now() >= deadline {
            bail!("test_server did not answer {health} within {startup:?}");
        }
        sleep(Duration::from_millis(100)).await;
    }
    eprintln!("Test server started on port {port}");
    Ok(server)
}
