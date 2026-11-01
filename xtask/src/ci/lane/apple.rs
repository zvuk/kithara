use std::{path::Path, process::Output};

use anyhow::{Context, Result};
use kithara_devtools::{common::tools::ToolsConfig, verdict::ChildFailure};
use tracing::{info, warn};

use crate::{
    child,
    ci::{config::CiConfig, process::Process, xcresult},
    consts,
    test_server::{Port, TestServer},
};

/// The two Apple lanes a parameter cannot describe. Both hold something open
/// for the length of a run - a package cache, a server, a simulator - and one
/// of them answers with the test's outcome rather than the build's.
fn preflight(process: &Process, tools: &ToolsConfig) -> Result<()> {
    process.require_os(&["macos"], "Apple")?;
    let xcodebuild = tools.program("xcodebuild");
    process.require_tools(&[
        "cargo",
        tools.program("just"),
        tools.program("kache"),
        tools.program("sccache"),
        tools.program("swift"),
        xcodebuild,
        tools.program("xcodegen"),
        tools.program("xcrun"),
    ])?;
    let version = process.capture(xcodebuild, &["-version"], "xcodebuild -version")?;
    let actual = version
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("Xcode "))
        .context("xcodebuild -version did not report an Xcode version")?;
    info!(xcode = actual, "Apple toolchain selected");
    process.run(
        xcodebuild,
        &["-checkFirstLaunchStatus"],
        "Xcode first launch readiness",
    )?;
    Ok(())
}

pub(crate) fn swift_test(
    process: &Process,
    tools: &ToolsConfig,
    swiftpm_cache: &Path,
) -> Result<()> {
    preflight(process, tools)?;
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
        .arg("--disable-xctest")
        .arg("--cache-path")
        .arg(swiftpm_cache)
        .arg("--xunit-output")
        .arg(&report);
    process.run_command(&mut command, "Swift package tests")
}

/// Run simulator tests on a device of their own, with an owned fixture server
/// and cancellation cleanup.
pub(crate) fn ios_test(
    process: &Process,
    config: &CiConfig,
    tools: &ToolsConfig,
    device_name: &str,
) -> Result<()> {
    preflight(process, tools)?;
    let cancel = child::Cancel::install()?;
    let server = TestServer::start(
        process,
        Port::Fixed(consts::TEST_SERVER_PORT),
        &process.root().join("target/xcresult/test-server.log"),
        Some(&cancel),
    )?;
    let simulator = Simulator::create(
        process,
        tools.program("xcrun"),
        device_name,
        &config.pins.ios_simulator_device_type,
    )?;
    let mut command = process.command(tools.program("just"));
    command
        .env("KITHARA_IOS_DESTINATION", simulator.destination())
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
    let deleted = simulator.delete();
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
    outcome.and(stopped).and(deleted)
}

/// A simulator the lane created for one run. A device keeps every app
/// container a reinstall retires and every symbol cache a run grows, so it
/// lives exactly as long as the run: deleted when the run ends, and replaced by
/// name when a killed run could not delete it.
struct Simulator<'a> {
    process: &'a Process,
    xcrun: &'a str,
    uuid: String,
    live: bool,
}

impl<'a> Simulator<'a> {
    fn create(process: &'a Process, xcrun: &'a str, name: &str, device_type: &str) -> Result<Self> {
        process.ensure(
            xcrun,
            &["simctl", "delete", name],
            "delete the simulator a killed run left",
            device_absent,
        )?;
        let uuid = process.capture(
            xcrun,
            &["simctl", "create", name, device_type],
            "create the run's simulator",
        )?;
        Ok(Self {
            process,
            xcrun,
            uuid: uuid.trim().to_owned(),
            live: true,
        })
    }

    /// The `xcodebuild` destination naming this device and no other.
    fn destination(&self) -> String {
        format!("platform=iOS Simulator,id={}", self.uuid)
    }

    /// [`Drop`] does the same, logging rather than returning, for a run that
    /// leaves early.
    fn delete(mut self) -> Result<()> {
        self.take_down()
    }

    fn take_down(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.live) {
            return Ok(());
        }
        self.process.run(
            self.xcrun,
            &["simctl", "delete", &self.uuid],
            "delete the run's simulator",
        )
    }
}

impl Drop for Simulator<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.take_down() {
            warn!(%error, uuid = %self.uuid, "the run's simulator was not deleted");
        }
    }
}

/// `simctl` refuses to delete a device that does not exist, which is the state
/// the delete is there to reach.
fn device_absent(output: &Output) -> bool {
    output.status.code() == Some(consts::SIMCTL_INVALID_DEVICE_EXIT)
        && String::from_utf8_lossy(&output.stderr).contains(consts::SIMCTL_INVALID_DEVICE)
}

fn build_xcframework(process: &Process, tools: &ToolsConfig) -> Result<()> {
    process.run(
        tools.program("just"),
        &["platform", "apple", "xcframework", "--profile", "debug"],
        "Apple XCFramework",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
    };

    use kithara_devtools::common::tools::ToolsConfig;

    use self::consts::UUID;
    use super::{Simulator, ios_test};
    use crate::{
        ci::{
            config::fixture,
            process::{Process, Recording},
        },
        testing::{install_script, signals},
    };

    mod consts {
        pub(super) const UUID: &str = "5F1C0B6E-0000-4000-8000-000000000001";
    }

    #[test]
    fn an_initialized_xcode_is_not_rejected_for_its_version() {
        let directory = tempfile::tempdir().unwrap();
        let process = Process::recording(
            directory.path(),
            Recording::default().with_reply(
                &["xcodebuild", "-version"],
                "Xcode 99.1\nBuild version 99A1\n",
            ),
        );
        super::preflight(&process, &ToolsConfig::default()).unwrap();
        let recording = process.recorded().unwrap();
        assert!(
            recording
                .steps()
                .iter()
                .any(|step| step.args == ["-checkFirstLaunchStatus"])
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_uninitialized_xcode_stops_before_building() {
        let _signals = signals();
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        for tool in [
            "cargo",
            "just",
            "kache",
            "sccache",
            "swift",
            "xcodebuild",
            "xcodegen",
            "xcrun",
        ] {
            crate::testing::install_double(&bin, tool);
        }
        let process = Process::new(
            directory.path(),
            BTreeMap::from([
                ("PATH".into(), bin.into_os_string()),
                (
                    "KITHARA_TEST_STDOUT".into(),
                    "Xcode 99.1\nBuild version 99A1".into(),
                ),
                (
                    "KITHARA_TEST_RULES".into(),
                    "xcodebuild:-checkFirstLaunchStatus:*=69".into(),
                ),
            ]),
        );
        let error = super::preflight(&process, &ToolsConfig::default()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Xcode first launch readiness failed (exit code 69)"),
            "{error:#}"
        );
    }

    #[test]
    fn ios_device_exclusion_covers_the_entire_job() {
        let apple: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(include_str!("../../../../.gitlab/ci/apple.yml")).unwrap();
        let common: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(include_str!("../../../../.gitlab/ci/common.yml")).unwrap();
        let extends = apple["apple:ios-test"]["extends"].as_sequence().unwrap();
        for owner in [".apple-device-job", ".measured"] {
            assert!(extends.iter().any(|value| value.as_str() == Some(owner)));
        }
        assert_eq!(
            common[".apple-device-job"]["tags"][0].as_str(),
            Some("kithara-macos-2")
        );
        assert_eq!(
            common[".measured"]["resource_group"].as_str(),
            Some("kithara-suite")
        );
    }

    #[test]
    fn ios_lane_keeps_its_fixed_server_and_deletes_its_device_before_returning() {
        let _signals = signals();
        let directory = tempfile::tempdir().unwrap();
        let config = fixture();
        let process = Process::recording(
            directory.path(),
            Recording::default()
                .with_reply(
                    &["xcodebuild", "-version"],
                    "Xcode 99.1\nBuild version 99A1",
                )
                .with_reply(&["xcrun", "simctl", "create"], UUID),
        );

        ios_test(&process, &config, &ToolsConfig::default(), "project-lane").unwrap();

        let recording = process.recorded().unwrap();
        let steps = recording.steps();
        let server = steps
            .iter()
            .position(|step| step.env.contains_key("TEST_SERVER_PORT"))
            .unwrap();
        assert_eq!(
            steps[server].env["TEST_SERVER_PORT"],
            crate::consts::TEST_SERVER_PORT.to_string()
        );
        let suite = steps
            .iter()
            .position(|step| step.args == ["platform", "apple", "test", "--skip-build"])
            .unwrap();
        assert!(server < suite);
        assert_eq!(
            steps[suite].env["KITHARA_IOS_DESTINATION"],
            format!("platform=iOS Simulator,id={UUID}")
        );
        assert_eq!(
            steps[suite].env["KITHARA_TEST_SERVER_URL"],
            format!("http://127.0.0.1:{}", crate::consts::TEST_SERVER_PORT)
        );
        let last = steps.last().unwrap();
        assert_eq!(last.program, "xcrun");
        assert_eq!(last.args, ["simctl", "delete", UUID]);
        assert!(suite < steps.len() - 1);
    }

    /// An `xcrun` that creates one device and answers a delete the way
    /// `simctl` does: the created device goes, an unknown name is an invalid
    /// device, and a `refused-` name is a refusal of any other kind.
    fn xcrun(directory: &Path) -> (PathBuf, PathBuf) {
        let trace = directory.join("trace");
        let program = directory.join("xcrun");
        install_script(
            &program,
            &format!(
                "#!/bin/sh\n\
                 echo \"$*\" >> '{trace}'\n\
                 case \"$2\" in\n\
                 create) echo {UUID} ;;\n\
                 delete) case \"$3\" in\n\
                 {UUID}) ;;\n\
                 refused-*) echo 'Unable to delete a device in use' >&2; exit 1 ;;\n\
                 *) echo \"Invalid device: $3\" >&2; exit 148 ;;\n\
                 esac ;;\n\
                 esac\n",
                trace = trace.display()
            ),
        );
        (program, trace)
    }

    /// No earlier run left a device, which `simctl` answers with a refusal;
    /// the run still gets a device of its own, and deletes exactly that one.
    #[test]
    fn a_run_owns_a_fresh_simulator_and_deletes_it() {
        let directory = tempfile::tempdir().unwrap();
        let (program, trace) = xcrun(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());

        let simulator =
            Simulator::create(&process, program.to_str().unwrap(), "project-lane", "type").unwrap();
        let destination = simulator.destination();
        simulator.delete().unwrap();

        assert_eq!(destination, format!("platform=iOS Simulator,id={UUID}"));
        assert_eq!(
            fs::read_to_string(&trace).unwrap(),
            format!(
                "simctl delete project-lane\n\
                 simctl create project-lane type\n\
                 simctl delete {UUID}\n"
            )
        );
    }

    /// A run that leaves early still gives its device back.
    #[test]
    fn a_simulator_dropped_without_a_delete_is_still_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let (program, trace) = xcrun(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());

        drop(
            Simulator::create(&process, program.to_str().unwrap(), "project-lane", "type").unwrap(),
        );

        let trace = fs::read_to_string(&trace).unwrap();
        assert!(
            trace.ends_with(&format!("simctl delete {UUID}\n")),
            "{trace}"
        );
    }

    /// Only an absent device lets the run go on: a leftover that refuses to go
    /// would sit next to the new one under the same name.
    #[test]
    fn a_leftover_that_refuses_to_go_stops_the_run() {
        let directory = tempfile::tempdir().unwrap();
        let (program, trace) = xcrun(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());

        let created =
            Simulator::create(&process, program.to_str().unwrap(), "refused-lane", "type");

        assert!(created.is_err());
        assert_eq!(
            fs::read_to_string(&trace).unwrap(),
            "simctl delete refused-lane\n"
        );
    }
}
