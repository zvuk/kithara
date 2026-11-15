use std::{collections::BTreeMap, env, fs, process::Command};

use anyhow::{Context, Result, bail};
use kithara_devtools::{Ctx, common::tools::ToolsConfig};
use regex::Regex;
use serde::Deserialize;

use super::AppleCommand;
use crate::ci::process::Process;

pub(super) fn run(command: AppleCommand, ctx: &Ctx) -> Result<()> {
    execute(
        command,
        &Process::new(&ctx.root, BTreeMap::new()),
        &ctx.config.tools,
    )
}

fn execute(command: AppleCommand, process: &Process, tools: &ToolsConfig) -> Result<()> {
    match command {
        AppleCommand::Xcode => {
            prepare(false, process, tools)?;
            process.run(
                tools.program("open"),
                &["apple/Examples/KitharaDemo/KitharaDemo.xcodeproj"],
                "open demo project",
            )
        }
        AppleCommand::Ios {
            scheme,
            destination,
        } => {
            prepare(false, process, tools)?;
            process.run_command(
                xcodebuild(process, tools, &scheme, &destination).args(["ARCHS=arm64", "build"]),
                "build iOS demo",
            )
        }
        AppleCommand::Test { skip_build } => {
            let destination = ios_destination(process, tools)?;
            let server = test_server(process)?;
            prepare(skip_build, process, tools)?;
            let bundle = process.root().join("target/xcresult/ios-test.xcresult");
            if bundle.exists() {
                fs::remove_dir_all(&bundle).context("remove previous iOS result bundle")?;
            }
            fs::create_dir_all(process.root().join("target/xcresult"))?;
            process.run_command(
                xcodebuild(process, tools, "KitharaDemoUnitTests_iOS", &destination)
                    .args([
                        "test",
                        "-resultBundlePath",
                        "target/xcresult/ios-test.xcresult",
                        "ARCHS=arm64",
                        "SWIFT_ENABLE_EXPLICIT_MODULES=NO",
                    ])
                    .env("TEST_RUNNER_KITHARA_TEST_SERVER_URL", server),
                "test iOS unit suite",
            )
        }
        AppleCommand::IntegrationRegressions { name } => {
            integration_regressions(name.as_deref(), process, tools)
        }
        AppleCommand::Doc { hosting_base } => {
            process.run(
                tools.program("just"),
                &["_xtask", "apple", "docgen"],
                "generate Apple documentation extensions",
            )?;
            fs::create_dir_all(process.root().join("docs-build"))?;
            let mut command = process.command(tools.program("swift"));
            command
                .args([
                    "package",
                    "--allow-writing-to-directory",
                    "./docs-build",
                    "generate-documentation",
                    "--target",
                    "Kithara",
                    "--target",
                    "KitharaRx",
                    "--enable-experimental-combined-documentation",
                    "--output-path",
                    "./docs-build/Kithara.doccarchive",
                ])
                .env("KITHARA_LOCAL_DEV", "1");
            if let Some(base) = hosting_base {
                command.args([
                    "--transform-for-static-hosting",
                    "--hosting-base-path",
                    &base,
                ]);
            }
            process.run_command(&mut command, "build Swift documentation")
        }
        _ => bail!("Apple build commands must use their build owner"),
    }
}

fn prepare(skip_build: bool, process: &Process, tools: &ToolsConfig) -> Result<()> {
    if skip_build {
        if !process
            .root()
            .join("apple/KitharaFFIInternal.xcframework")
            .is_dir()
        {
            bail!("apple/KitharaFFIInternal.xcframework is missing; drop --skip-build or fetch it");
        }
    } else {
        process.run(
            tools.program("just"),
            &["_xtask", "apple", "build", "--profile", "debug"],
            "build debug Apple framework",
        )?;
    }
    generate_project(process, tools)
}

fn generate_project(process: &Process, tools: &ToolsConfig) -> Result<()> {
    let project = process
        .root()
        .join("apple/Examples/KitharaDemo/project.yml");
    let original = fs::read_to_string(&project).context("read demo project specification")?;
    let local = original
        .lines()
        .filter(|line| !line.contains("from:"))
        .map(|line| line.replace("url: https://github.com/zvuk/kithara", "path: ../../.."))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&project, local).context("select local Swift package")?;
    let result = (|| {
        process.run_command(
            process
                .command_in(tools.program("xcrun"), "apple/Examples/KitharaDemo")
                .args(["--sdk", "macosx", "swift", "scripts/bake-secrets.swift"])
                .env_remove("SDKROOT"),
            "bake demo secrets",
        )?;
        process.run_command(
            process
                .command_in(tools.program("xcodegen"), "apple/Examples/KitharaDemo")
                .arg("generate")
                .env_remove("SDKROOT")
                .env("KITHARA_LOCAL_DEV", "1"),
            "generate local demo project",
        )
    })();
    fs::write(&project, original).context("restore demo project specification")?;
    result
}

fn xcodebuild(process: &Process, tools: &ToolsConfig, scheme: &str, destination: &str) -> Command {
    let mut command = process.command(tools.program("xcodebuild"));
    command
        .args([
            "-project",
            "apple/Examples/KitharaDemo/KitharaDemo.xcodeproj",
            "-scheme",
            scheme,
            "-destination",
            destination,
            "-derivedDataPath",
        ])
        .arg(
            process
                .root()
                .join("apple/Examples/KitharaDemo/build/DerivedData"),
        )
        .env("KITHARA_LOCAL_DEV", "1");
    command
}

fn test_server(process: &Process) -> Result<std::ffi::OsString> {
    process
        .environment_var("KITHARA_TEST_SERVER_URL")
        .filter(|url| !url.is_empty())
        .context("KITHARA_TEST_SERVER_URL must name the running hermetic test server")
}

fn integration_regressions(
    name: Option<&str>,
    process: &Process,
    tools: &ToolsConfig,
) -> Result<()> {
    let destination = ios_destination(process, tools)?;
    let server = test_server(process)?;
    let mut selection =
        "-only-testing:KitharaDemoUnitTests_iOS/IntegrationRegressionsIOS".to_owned();
    let mut filename = "kithara-integration-regressions.log".to_owned();
    if let Some(name) = name {
        let known = regression_names(process)?;
        if !known.iter().any(|test| test == name) {
            bail!(
                "unknown integration regression test: {name}\nknown tests:\n{}",
                known.join("\n")
            );
        }
        selection.push_str(&format!("/{name}()"));
        filename = format!("kithara-integration-regressions-{name}.log");
    }
    prepare(false, process, tools)?;
    process.run_command(
        xcodebuild(process, tools, "KitharaDemoUnitTests_iOS", &destination).args([
            "build-for-testing",
            "ARCHS=arm64",
            "SWIFT_ENABLE_EXPLICIT_MODULES=NO",
        ]),
        "build iOS integration regressions",
    )?;
    let log = process
        .environment_path("TMPDIR")
        .unwrap_or_else(env::temp_dir)
        .join(filename);
    let file = fs::File::create(&log).context("create integration regression log")?;
    let result = process.run_command(
        xcodebuild(process, tools, "KitharaDemoUnitTests_iOS", &destination)
            .args(["test-without-building", &selection])
            .env("TEST_RUNNER_KITHARA_TEST_SERVER_URL", server)
            .stdout(file.try_clone()?)
            .stderr(file),
        "test iOS integration regressions",
    );
    let output = fs::read_to_string(&log).context("read integration regression log")?;
    for line in output.lines().filter(|line| {
        line.contains('\u{2714}') || line.contains('\u{2718}') || line.contains('\u{21b3}')
    }) {
        println!("{line}");
    }
    println!("log: {}", log.display());
    result
}

fn regression_names(process: &Process) -> Result<Vec<String>> {
    let pattern = Regex::new(r"@Test\([^\n]*\n[^\n]*func ([A-Za-z0-9]+)")?;
    let mut names = Vec::new();
    for path in
        super::command::swift_files(&process.root().join("apple/Examples/KitharaDemo/UnitTests"))?
    {
        let source = fs::read_to_string(path)?;
        if source.contains("IntegrationRegressionsIOS") {
            names.extend(
                pattern
                    .captures_iter(&source)
                    .map(|capture| capture[1].to_owned()),
            );
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

#[derive(Deserialize)]
struct Simulators {
    devices: BTreeMap<String, Vec<Device>>,
    runtimes: Vec<Runtime>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    identifier: String,
    version: String,
    #[serde(default)]
    is_available: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    device_type_identifier: String,
    name: String,
    state: String,
    #[serde(default)]
    is_available: bool,
}

fn ios_destination(process: &Process, tools: &ToolsConfig) -> Result<String> {
    if let Some(destination) = process
        .environment_var("KITHARA_IOS_DESTINATION")
        .filter(|value| !value.is_empty())
    {
        return destination
            .into_string()
            .map_err(|_| anyhow::anyhow!("KITHARA_IOS_DESTINATION is not UTF-8"));
    }
    let json = process.capture(
        tools.program("xcrun"),
        &["simctl", "list", "--json"],
        "list iOS simulators",
    )?;
    select_destination(&json)
}

fn select_destination(json: &str) -> Result<String> {
    let inventory: Simulators = serde_json::from_str(json).context("parse simctl inventory")?;
    let mut candidates = Vec::new();
    for runtime in inventory
        .runtimes
        .iter()
        .filter(|runtime| runtime.is_available && runtime.identifier.contains(".SimRuntime.iOS-"))
    {
        if let Some(devices) = inventory.devices.get(&runtime.identifier) {
            for device in devices.iter().filter(|device| device.is_available) {
                candidates.push((
                    device.state != "Booted",
                    !device.device_type_identifier.contains("iPhone"),
                    &device.name,
                    &runtime.version,
                ));
            }
        }
    }
    let (_, _, name, version) = candidates.into_iter().min().context(
        "no usable iOS simulator; create an available device on an available iOS runtime in Xcode, or set KITHARA_IOS_DESTINATION",
    )?;
    Ok(format!("platform=iOS Simulator,name={name},OS={version}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::process::Recording;

    #[test]
    fn simulator_selection_keeps_booted_then_phone_then_name_priority() {
        let inventory = serde_json::json!({
            "runtimes": [
                {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-1", "version": "26.1", "isAvailable": true},
                {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-2", "version": "26.2", "isAvailable": false},
                {"identifier": "com.apple.CoreSimulator.SimRuntime.tvOS-26-1", "version": "26.1", "isAvailable": true}
            ],
            "devices": {
                "com.apple.CoreSimulator.SimRuntime.iOS-26-1": [
                    {"name": "A tablet", "state": "Booted", "deviceTypeIdentifier": "iPad", "isAvailable": true},
                    {"name": "Z phone", "state": "Booted", "deviceTypeIdentifier": "iPhone", "isAvailable": true},
                    {"name": "A phone", "state": "Shutdown", "deviceTypeIdentifier": "iPhone", "isAvailable": true},
                    {"name": "Unavailable", "state": "Booted", "deviceTypeIdentifier": "iPhone", "isAvailable": false}
                ],
                "com.apple.CoreSimulator.SimRuntime.iOS-26-2": [
                    {"name": "A newer phone", "state": "Booted", "deviceTypeIdentifier": "iPhone", "isAvailable": true}
                ],
                "com.apple.CoreSimulator.SimRuntime.tvOS-26-1": [
                    {"name": "A TV", "state": "Booted", "deviceTypeIdentifier": "iPhone", "isAvailable": true}
                ]
            }
        });
        assert_eq!(
            select_destination(&inventory.to_string()).unwrap(),
            "platform=iOS Simulator,name=Z phone,OS=26.1"
        );
        let mut inventory = inventory;
        inventory["devices"]["com.apple.CoreSimulator.SimRuntime.iOS-26-1"][2]["state"] =
            "Booted".into();
        assert_eq!(
            select_destination(&inventory.to_string()).unwrap(),
            "platform=iOS Simulator,name=A phone,OS=26.1"
        );
        assert!(
            select_destination("{\"runtimes\":[],\"devices\":{}}")
                .unwrap_err()
                .to_string()
                .contains("no usable iOS simulator")
        );
        assert!(select_destination("{}").is_err());
        assert!(select_destination("not json").is_err());
    }

    #[test]
    fn simulator_override_bypasses_inventory_and_preserves_destination() {
        let process = Process::new(
            std::path::Path::new("."),
            BTreeMap::from([(
                "KITHARA_IOS_DESTINATION".into(),
                "platform=iOS Simulator,id=owned".into(),
            )]),
        );
        let tools: ToolsConfig = serde_json::from_value(
            serde_json::json!({"xcrun": {"program": "/nonexistent/simctl"}}),
        )
        .unwrap();
        assert_eq!(
            ios_destination(&process, &tools).unwrap(),
            "platform=iOS Simulator,id=owned"
        );
    }

    #[test]
    fn apple_cli_keeps_just_modes_and_argument_validation() {
        use clap::Parser;

        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: AppleCommand,
        }

        assert!(matches!(
            Cli::try_parse_from(["apple", "xcframework"])
                .unwrap()
                .command,
            AppleCommand::Build { .. }
        ));
        assert!(matches!(
            Cli::try_parse_from(["apple", "demo"]).unwrap().command,
            AppleCommand::Run { .. }
        ));
        assert!(matches!(
            Cli::try_parse_from(["apple", "test", "--skip-build"])
                .unwrap()
                .command,
            AppleCommand::Test { skip_build: true }
        ));
        assert!(Cli::try_parse_from(["apple", "test", "--typo"]).is_err());
        assert!(Cli::try_parse_from(["apple", "unknown"]).is_err());
    }

    fn project(root: &std::path::Path) {
        let demo = root.join("apple/Examples/KitharaDemo");
        fs::create_dir_all(demo.join("UnitTests")).unwrap();
        fs::write(
            demo.join("project.yml"),
            "packages:\n  Kithara:\n    url: https://github.com/zvuk/kithara\n    from: 1.0.0\n",
        )
        .unwrap();
        fs::write(
            demo.join("UnitTests/Regression.swift"),
            "struct IntegrationRegressionsIOS {}\n@Test(\"regression\")\nfunc regression() {}\n",
        )
        .unwrap();
    }

    #[test]
    fn missing_framework_and_unknown_test_fail_before_any_build() {
        let root = tempfile::tempdir().unwrap();
        project(root.path());
        let process = Process::recording(root.path(), Recording::default());
        let tools = ToolsConfig::default();
        assert!(
            prepare(true, &process, &tools)
                .unwrap_err()
                .to_string()
                .contains("is missing")
        );
        assert!(process.recorded().unwrap().steps().is_empty());

        let process = Process::new(
            root.path(),
            BTreeMap::from([
                (
                    "KITHARA_IOS_DESTINATION".into(),
                    "platform=iOS Simulator,id=owned".into(),
                ),
                (
                    "KITHARA_TEST_SERVER_URL".into(),
                    "http://127.0.0.1:1".into(),
                ),
            ]),
        );
        assert!(
            integration_regressions(Some("../../invalid"), &process, &tools)
                .unwrap_err()
                .to_string()
                .contains("unknown integration regression test")
        );
    }

    #[test]
    fn documentation_keeps_both_targets_and_optional_static_hosting() {
        let root = tempfile::tempdir().unwrap();
        for hosting_base in [None, Some("kithara/apple".to_owned())] {
            let process = Process::recording(root.path(), Recording::default());
            execute(
                AppleCommand::Doc {
                    hosting_base: hosting_base.clone(),
                },
                &process,
                &ToolsConfig::default(),
            )
            .unwrap();
            let recording = process.recorded().unwrap();
            assert_eq!(recording.steps().len(), 2);
            assert_eq!(recording.steps()[0].args, ["_xtask", "apple", "docgen"]);
            let step = &recording.steps()[1];
            assert_eq!(step.program, "swift");
            assert_eq!(step.env["KITHARA_LOCAL_DEV"], "1");
            assert!(
                step.args
                    .windows(4)
                    .any(|args| args == ["--target", "Kithara", "--target", "KitharaRx"])
            );
            assert_eq!(
                step.args
                    .iter()
                    .any(|arg| arg == "--transform-for-static-hosting"),
                hosting_base.is_some()
            );
            if let Some(base) = hosting_base {
                assert!(
                    step.args
                        .ends_with(&["--hosting-base-path".to_owned(), base])
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn xcode_recipes_keep_derived_data_in_each_job_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let bin = base.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for tool in ["just", "xcrun"] {
            crate::testing::install_script(&bin.join(tool), "#!/bin/sh\nexit 0\n");
        }
        crate::testing::install_script(
            &bin.join("xcodegen"),
            "#!/bin/sh\nset -eu\ntest -z \"${SDKROOT:-}\"\ngrep -q 'path: ../../..' project.yml\n! grep -q 'from:' project.yml\nexit \"${PROBE_XCODEGEN_STATUS:-0}\"\n",
        );
        crate::testing::install_script(
            &bin.join("xcodebuild"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> xcode-commands
for arg in "$@"; do
    if [ "$arg" = test ] || [ "$arg" = test-without-building ]; then
        test "$TEST_RUNNER_KITHARA_TEST_SERVER_URL" = http://127.0.0.1:1
    fi
done
while [ "$#" -gt 0 ]; do
    if [ "$1" = -derivedDataPath ]; then
        derived=$2
        shift
    fi
    shift
done
test "$derived" = "$PWD/apple/Examples/KitharaDemo/build/DerivedData"
mkdir -p "$derived"
cp revision "$derived/producer"
printf '%s\n' "$derived" >> xcode-paths
exit "${PROBE_XCODE_STATUS:-0}"
"#,
        );
        let tools: ToolsConfig = serde_json::from_value(serde_json::json!({
            "just": {"program": bin.join("just")},
            "xcrun": {"program": bin.join("xcrun")},
            "xcodegen": {"program": bin.join("xcodegen")},
            "xcodebuild": {"program": bin.join("xcodebuild")}
        }))
        .unwrap();
        for (job, revision) in [("job a", "branch-a"), ("job b", "branch-b")] {
            let root = base.join(job);
            project(&root);
            fs::create_dir_all(root.join("apple/KitharaFFIInternal.xcframework")).unwrap();
            fs::write(root.join("revision"), revision).unwrap();
            let vars = BTreeMap::from([
                ("TMPDIR".into(), root.clone().into_os_string()),
                ("SDKROOT".into(), "inherited-ios-sdk".into()),
                (
                    "KITHARA_IOS_DESTINATION".into(),
                    "platform=iOS Simulator,id=owned".into(),
                ),
                (
                    "KITHARA_TEST_SERVER_URL".into(),
                    "http://127.0.0.1:1".into(),
                ),
            ]);
            let process = Process::new(&root, vars.clone());
            let spec = root.join("apple/Examples/KitharaDemo/project.yml");
            let original = fs::read(&spec).unwrap();
            for command in [
                AppleCommand::Ios {
                    scheme: "KitharaDemo_iOS".into(),
                    destination: "generic/platform=iOS Simulator".into(),
                },
                AppleCommand::Test { skip_build: true },
                AppleCommand::IntegrationRegressions { name: None },
                AppleCommand::IntegrationRegressions {
                    name: Some("regression".into()),
                },
            ] {
                execute(command, &process, &tools).unwrap();
                assert_eq!(fs::read(&spec).unwrap(), original);
            }
            let derived = root.join("apple/Examples/KitharaDemo/build/DerivedData");
            assert_eq!(
                fs::read_to_string(derived.join("producer")).unwrap(),
                revision
            );
            assert_eq!(
                fs::read_to_string(root.join("xcode-paths")).unwrap(),
                format!("{}\n", derived.display()).repeat(6)
            );
            let commands = fs::read_to_string(root.join("xcode-commands")).unwrap();
            assert!(commands.contains("-resultBundlePath target/xcresult/ios-test.xcresult ARCHS=arm64 SWIFT_ENABLE_EXPLICIT_MODULES=NO"));
            assert!(commands.contains(
                "-only-testing:KitharaDemoUnitTests_iOS/IntegrationRegressionsIOS/regression()"
            ));

            let mut failed = vars.clone();
            failed.insert("PROBE_XCODE_STATUS".into(), "7".into());
            assert!(
                execute(
                    AppleCommand::Test { skip_build: true },
                    &Process::new(&root, failed),
                    &tools
                )
                .is_err()
            );
            let mut failed = vars;
            failed.insert("PROBE_XCODEGEN_STATUS".into(), "9".into());
            assert!(generate_project(&Process::new(&root, failed), &tools).is_err());
            assert_eq!(fs::read(spec).unwrap(), original);
        }
    }
}
