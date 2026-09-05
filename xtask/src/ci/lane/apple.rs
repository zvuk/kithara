use std::{
    net::{SocketAddr, TcpStream},
    path::Path,
    process::Child,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::common::tools::ToolsConfig;

use crate::ci::{config::CiConfig, process::Process, xcresult};

/// The two Apple lanes a parameter cannot describe. Both hold something open
/// for the length of a run - a package cache, a server, a simulator - and one
/// of them answers with the test's outcome rather than the build's.
fn preflight(process: &Process, config: &CiConfig, tools: &ToolsConfig) -> Result<()> {
    process.require_os("macos", "Apple")?;
    let xcodebuild = tools.program("xcodebuild");
    process.require_tools(&[
        "cargo",
        tools.program("just"),
        tools.program("sccache"),
        tools.program("swift"),
        xcodebuild,
        tools.program("xcodegen"),
    ])?;
    let version = process.capture(xcodebuild, &["-version"], "xcodebuild -version")?;
    let actual = version
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("Xcode "))
        .context("xcodebuild -version did not report an Xcode version")?;
    if actual != config.pins.expected_xcode_version {
        bail!(
            "Xcode {} is required, found {actual}",
            config.pins.expected_xcode_version
        );
    }
    Ok(())
}

pub(crate) fn swift_test(
    process: &Process,
    config: &CiConfig,
    tools: &ToolsConfig,
    swiftpm_cache: &Path,
) -> Result<()> {
    preflight(process, config, tools)?;
    // The Swift package resolves the framework from the debug build tree, so
    // this job builds it too. Repeated work is nearly free — the jobs share a
    // target directory on the executor — and it keeps the job self-contained.
    build_xcframework(process, tools)?;
    // SwiftPM writes xUnit on request, so this lane needs no conversion.
    let report = process.root().join("target/xcresult/swift-test.junit.xml");
    if let Some(parent) = report.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut command = process.command(tools.program("swift"));
    command
        .env("KITHARA_LOCAL_DEV", "1")
        .arg("test")
        .arg("--cache-path")
        .arg(swiftpm_cache)
        .arg("--xunit-output")
        .arg(&report);
    process.run_command(&mut command, "Swift package tests")
}

/// The `XCTest` suite on a simulator. It reads its fixtures from a hermetic
/// server rather than from the network, and nothing else in the job starts
/// one, so this lane owns its lifetime: the guard stops the server however the
/// job ends, because one left holding the port fails the next run with nothing
/// more informative than a bind error.
pub(crate) fn ios_test(process: &Process, config: &CiConfig, tools: &ToolsConfig) -> Result<()> {
    preflight(process, config, tools)?;
    let server = TestServer::start(process)?;
    let mut command = process.command(tools.program("just"));
    command
        .env("KITHARA_LOCAL_DEV", "1")
        .env("KITHARA_TEST_SERVER_URL", server.url())
        // The framework comes from the job that builds it. This one holds the
        // measured group while a simulator runs, and rebuilding what another
        // job already produced spends that window twice.
        .args(["platform", "apple", "test", "--skip-build"]);
    let outcome = process.run_command(&mut command, "iOS Simulator tests");
    // A failing run is exactly the one whose report matters, so the bundle is
    // converted either way and the test outcome is returned afterwards.
    let bundle = process.root().join("target/xcresult/ios-test.xcresult");
    if bundle.exists() {
        xcresult::write_junit(
            process,
            tools.program("xcrun"),
            &bundle,
            &process.root().join("target/xcresult/ios-test.junit.xml"),
        )?;
    }
    outcome
}

struct Consts;

impl Consts {
    /// The simulator shares the host network stack, so it reaches the server
    /// over loopback. The port is fixed because Apple simulator suites are
    /// serialized on the host, so another CI lane cannot bind it concurrently.
    const TEST_SERVER_PORT: u16 = 3444;
    const TEST_SERVER_POLL: Duration = Duration::from_millis(200);
    const TEST_SERVER_READY: Duration = Duration::from_secs(60);
}

struct TestServer {
    /// Absent when the process only recorded the request to start one.
    child: Option<Child>,
    url: String,
}

impl TestServer {
    fn start(process: &Process) -> Result<Self> {
        process.run(
            "cargo",
            &[
                "build",
                "-p",
                "kithara-integration-tests",
                "--bin",
                "test_server",
            ],
            "hermetic test server",
        )?;
        let binary = process.target_dir().join("debug/test_server");
        let mut command = process.command(&binary);
        command.env("TEST_SERVER_PORT", Consts::TEST_SERVER_PORT.to_string());
        let child = process.spawn(&mut command, "hermetic test server")?;
        let server = Self {
            child,
            url: format!("http://127.0.0.1:{}", Consts::TEST_SERVER_PORT),
        };
        if server.child.is_some() {
            wait_until_listening()?;
        }
        Ok(server)
    }

    fn url(&self) -> &str {
        &self.url
    }
}

/// The server binds its socket last, so a connection that completes means it
/// is answering.
fn wait_until_listening() -> Result<()> {
    let address = SocketAddr::from(([127, 0, 0, 1], Consts::TEST_SERVER_PORT));
    let deadline = Instant::now() + Consts::TEST_SERVER_READY;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address, Consts::TEST_SERVER_POLL).is_ok() {
            return Ok(());
        }
        thread::sleep(Consts::TEST_SERVER_POLL);
    }
    bail!(
        "the hermetic test server did not listen on {address} within {}s",
        Consts::TEST_SERVER_READY.as_secs()
    )
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn build_xcframework(process: &Process, tools: &ToolsConfig) -> Result<()> {
    process.run(
        tools.program("just"),
        &["platform", "apple", "xcframework", "--profile", "debug"],
        "Apple XCFramework",
    )
}
