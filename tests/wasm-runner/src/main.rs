//! `cargo test` runner for `wasm32-unknown-unknown` test binaries.
//!
//! Generates the bindings for one binary, serves them on a loopback page and
//! runs the tests in a headless browser over `WebDriver`, on the page or in a
//! dedicated worker as the binary requests.

mod capabilities;
mod driver;
mod fixture;
mod page;
mod server;
mod suite;

use std::{
    convert, env,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Result, bail};
use clap::Parser;
use kithara_platform::{
    time::{Duration, Instant, sleep, timeout},
    tokio,
};

use crate::{
    driver::Session,
    suite::{Filter, Suite},
};

/// The line the harness prints once every selected test has finished.
const SUMMARY: &str = "test result: ";
const POLL: Duration = Duration::from_millis(100);
const PAGE_READY: Duration = Duration::from_secs(5);

/// The flags cargo forwards after `--`.
#[derive(Parser)]
struct Cli {
    /// The Wasm test binary; cargo passes it.
    file: PathBuf,
    /// Run only tests whose names contain this.
    filter: Option<String>,
    /// Match the filter against the whole name.
    #[arg(long)]
    exact: bool,
    /// Print the selected test names instead of running them.
    #[arg(long)]
    list: bool,
    /// Run ignored tests too.
    #[arg(long)]
    include_ignored: bool,
    /// Stream console output with the results instead of only on failure.
    #[arg(long)]
    nocapture: bool,
}

fn seconds(name: &str, default: u64) -> Duration {
    Duration::from_secs(
        env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let watchdog = seconds(
        "WASM_TEST_RUNNER_TIMEOUT_SECS",
        if cli.filter.is_some() { 180 } else { 1800 },
    );
    match run(cli, watchdog).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the binary's selected tests within `watchdog`; `false` when any of
/// them failed. The browser session is closed however the run ends.
async fn run(cli: Cli, watchdog: Duration) -> Result<bool> {
    let deadline = Instant::now() + watchdog;
    let filter = Filter {
        pattern: cli.filter.clone(),
        exact: cli.exact,
    };
    let mut suite = Suite::read(&cli.file, &filter)?;
    if cli.list {
        for test in &suite.tests {
            println!("{}: test", test.name);
        }
        return Ok(true);
    }
    if suite.tests.is_empty() {
        println!("no tests to run!");
        return Ok(true);
    }
    let _fixtures = within(deadline, watchdog, fixture::ensure()).await??;
    let dir = tempfile::tempdir()?;
    suite.generate(dir.path())?;
    let (browser, driver) = driver::locate()?;
    let options = page::Options {
        nocapture: cli.nocapture,
        include_ignored: cli.include_ignored,
    };
    let index = page::write(dir.path(), &suite, options, browser.hidden_style())?;
    let server = server::spawn(dir.path().to_path_buf(), index).await;
    let capabilities = capabilities::load(browser, &cli.file)?;
    let session = within(
        deadline,
        watchdog,
        Session::start(browser, driver, capabilities),
    )
    .await??;
    let url = server.url("/").to_string();
    eprintln!("Running headless tests in {browser:?} at {url}");
    let outcome = within(deadline, watchdog, drive(&session, &url, deadline))
        .await
        .and_then(convert::identity);
    session.close().await;
    outcome
}

/// `future`'s output, unless the run's `deadline` passes first.
async fn within<F: Future>(deadline: Instant, watchdog: Duration, future: F) -> Result<F::Output> {
    timeout(deadline.saturating_duration_since(Instant::now()), future)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "the browser run exceeded {watchdog:?}; WASM_TEST_RUNNER_TIMEOUT_SECS overrides it"
            )
        })
}

/// Opens the page and reports its results.
async fn drive(session: &Session, url: &str, watchdog: Instant) -> Result<bool> {
    session.visit(url).await?;
    activate(session).await?;
    let limit = seconds("WASM_BINDGEN_TEST_TIMEOUT", 20).min(
        watchdog
            .saturating_duration_since(Instant::now())
            .saturating_sub(POLL),
    );
    report(session, limit).await
}

async fn activate(session: &Session) -> Result<()> {
    let deadline = Instant::now() + PAGE_READY;
    while !session.tests_ready().await? {
        if Instant::now() >= deadline {
            bail!("the test page did not initialize within {PAGE_READY:?}");
        }
        sleep(POLL).await;
    }
    session.click("kithara-run-tests").await
}

/// Streams the page's output until the summary appears or `limit` passes.
async fn report(session: &Session, limit: Duration) -> Result<bool> {
    let deadline = Instant::now() + limit;
    let mut output = String::new();
    let mut offset = 0;
    loop {
        let (chunk, next) = session.text_since("output", offset, deadline).await?;
        offset = next;
        io::stdout().write_all(chunk.as_bytes())?;
        output.push_str(&chunk);
        if output.contains(SUMMARY) || Instant::now() >= deadline {
            break;
        }
        sleep(POLL).await;
    }
    if output.contains(&format!("{SUMMARY}ok")) {
        return Ok(true);
    }
    if !output.contains(SUMMARY) {
        println!(
            "Failed to detect test as having been run. It might have timed out after {limit:?}."
        );
    }
    let (console, _) = session.text_since("console_output", 0, deadline).await?;
    if !console.is_empty() {
        println!("console output:");
        for line in console.lines() {
            println!("    {line}");
        }
    }
    if output.contains(SUMMARY) {
        return Ok(false);
    }
    bail!("the page never reported a result")
}
