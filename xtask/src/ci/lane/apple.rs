use std::path::Path;

use anyhow::{Context, Result, bail};
use kithara_devtools::{common::tools::ToolsConfig, verdict::ChildFailure};

use crate::{
    child,
    ci::{config::CiConfig, process::Process, xcresult},
    test_server::{Port, TestServer},
};

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

/// Run simulator tests with an owned fixture server and cancellation cleanup.
pub(crate) fn ios_test(process: &Process, config: &CiConfig, tools: &ToolsConfig) -> Result<()> {
    preflight(process, config, tools)?;
    let cancel = child::Cancel::install()?;
    let server = TestServer::start(
        process,
        Port::Fixed(Consts::TEST_SERVER_PORT),
        &process.root().join("target/xcresult/test-server.log"),
        Some(&cancel),
    )?;
    let mut command = process.command(tools.program("just"));
    command
        .env("KITHARA_LOCAL_DEV", "1")
        .env("KITHARA_TEST_SERVER_URL", server.url())
        // The framework comes from the job that builds it. This one holds the
        // measured group while a simulator runs, and rebuilding what another
        // job already produced spends that window twice.
        .args(["platform", "apple", "test", "--skip-build"]);
    child::isolate(&mut command);
    let outcome = process
        .spawn(&mut command, "iOS Simulator tests")
        .and_then(|child| {
            let Some(mut child) = child else {
                return Ok(());
            };
            let status = child::supervise(&mut child, Some(&cancel), None)?;
            if !status.success() {
                return Err(ChildFailure::inherited(
                    "iOS Simulator tests".to_owned(),
                    status.code(),
                ));
            }
            Ok(())
        });
    let stopped = server.stop();
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
    outcome.and(stopped)
}

struct Consts;

impl Consts {
    /// The simulator shares the host network stack, so it reaches the server
    /// over loopback. The port is fixed because Apple simulator suites are
    /// serialized on the host, so another CI lane cannot bind it concurrently.
    const TEST_SERVER_PORT: u16 = 3444;
}

fn build_xcframework(process: &Process, tools: &ToolsConfig) -> Result<()> {
    process.run(
        tools.program("just"),
        &["platform", "apple", "xcframework", "--profile", "debug"],
        "Apple XCFramework",
    )
}
