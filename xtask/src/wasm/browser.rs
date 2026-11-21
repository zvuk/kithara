use std::{collections::BTreeMap, env, ffi::OsString, path::Path};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use kithara_devtools::{Ctx, util::nightly_toolchain, verdict::ChildFailure};
use serde::Deserialize;
use serde_json::Value;

use crate::{ci::process::Process, consts};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum Browser {
    #[default]
    Chrome,
    Firefox,
    Safari,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum Suite {
    #[default]
    All,
    #[value(name = "webcodecs")]
    WebCodecs,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum AudioRoute {
    Include,
    #[default]
    Exclude,
}

#[derive(Debug, Args)]
pub(crate) struct TestArgs {
    #[arg(value_enum, default_value_t)]
    browser: Browser,
    #[arg(value_enum, default_value_t)]
    suite: Suite,
    #[arg(value_enum, default_value_t)]
    audio_route: AudioRoute,
}

#[derive(Debug, Deserialize)]
struct LibraryTests {
    #[serde(rename = "default-features")]
    default_features: bool,
    features: Vec<String>,
}

pub(super) fn run(args: &TestArgs, ctx: &Ctx) -> Result<()> {
    execute(args, ctx, &Process::new(&ctx.root, BTreeMap::new()))
}

fn libraries<'a>(
    packages: impl Iterator<Item = (&'a str, &'a Value)>,
) -> Result<Vec<(String, LibraryTests)>> {
    let mut selected = Vec::new();
    for (name, metadata) in packages {
        let Some(entry) = metadata
            .pointer("/kithara/wasm-lib-tests")
            .filter(|entry| !entry.is_null())
        else {
            continue;
        };
        let settings = serde_json::from_value(entry.clone()).with_context(|| {
            format!("{name}: [package.metadata.kithara.wasm-lib-tests] needs a boolean default-features and a features list")
        })?;
        selected.push((name.to_owned(), settings));
    }
    Ok(selected)
}

fn environment(args: &TestArgs, target: &Path) -> Result<BTreeMap<OsString, OsString>> {
    let bin = target.join("debug");
    let path = env::join_paths(
        std::iter::once(bin).chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )
    .context("build browser test runner search path")?;
    let (key, program) = match args.browser {
        Browser::Chrome => ("CHROMEDRIVER", "chromedriver"),
        Browser::Firefox => ("GECKODRIVER", "geckodriver"),
        Browser::Safari => ("SAFARIDRIVER", "safaridriver"),
    };
    let driver = env::var_os(key)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| program.into());
    Ok(BTreeMap::from([
        (OsString::from("PATH"), path),
        (key.into(), driver),
        (
            "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER".into(),
            "wasm_test_runner".into(),
        ),
        ("WASM_BINDGEN_TEST_TIMEOUT".into(), "300".into()),
    ]))
}

/// The fixture binaries cargo reported building, keyed by the variable that
/// names each one to the runner's protocol test.
fn fixture_binaries(messages: &str) -> Result<BTreeMap<OsString, OsString>> {
    let mut binaries = BTreeMap::new();
    for message in messages
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let target = message.pointer("/target/name").and_then(Value::as_str);
        let executable = message.get("executable").and_then(Value::as_str);
        if let (Some(target), Some(executable)) = (target, executable)
            && let Some((_, variable)) = consts::WASM_RUNNER_FIXTURE_VARIABLES
                .iter()
                .find(|(name, _)| *name == target)
        {
            binaries.insert(OsString::from(variable), OsString::from(executable));
        }
    }
    if binaries.len() != consts::WASM_RUNNER_FIXTURE_VARIABLES.len() {
        anyhow::bail!("cargo did not report every runner protocol fixture binary: {binaries:?}");
    }
    Ok(binaries)
}

fn execute(args: &TestArgs, ctx: &Ctx, process: &Process) -> Result<()> {
    if args.suite == Suite::WebCodecs && args.browser != Browser::Chrome {
        return Err(ChildFailure::inherited(
            "the WebCodecs suite requires chrome".to_owned(),
            Some(2),
        ));
    }
    let metadata = ctx.metadata()?;
    let selected = libraries(
        metadata
            .workspace_packages()
            .into_iter()
            .map(|p| (p.name.as_str(), &p.metadata)),
    )?;
    let cargo = ctx.config.tools.program("cargo");
    process.run(
        cargo,
        &[
            "build",
            "-p",
            "kithara-integration-tests",
            "--features",
            "all",
            "--bin",
            "test_server",
        ],
        "build browser fixture server",
    )?;
    process.run(
        cargo,
        &[
            "build",
            "-p",
            "kithara-wasm-test-runner",
            "--bin",
            "wasm_test_runner",
        ],
        "build browser test runner",
    )?;

    let vars = environment(args, metadata.target_directory.as_std_path())?;
    let toolchain = format!("+{}", nightly_toolchain());
    let mut fixtures = process.command(cargo);
    fixtures
        .arg(&toolchain)
        .args(consts::WASM_TEST_ARGS)
        .args(consts::WASM_RUNNER_FIXTURES)
        .args(["--no-run", "--message-format=json"]);
    let built = process.capture_command(&mut fixtures, "build runner protocol fixtures")?;
    let mut protocol = process.command(cargo);
    protocol
        .args(consts::WASM_RUNNER_PROTOCOL_TEST)
        .envs(&vars)
        .envs(fixture_binaries(&built)?);
    process.run_command(&mut protocol, "wasm runner protocol")?;
    let run = |label: &str, selection: &[&str]| {
        let mut command = process.command(cargo);
        command
            .arg(&toolchain)
            .args(consts::WASM_TEST_ARGS)
            .args(selection)
            .args(["--", "--nocapture"])
            .envs(&vars);
        process.run_command(&mut command, label)
    };
    run("wasm encoder session", consts::WASM_ENCODER_TEST)?;
    for (package, settings) in &selected {
        let mut selection = vec!["-p", package.as_str(), "--lib"];
        if !settings.default_features {
            selection.push("--no-default-features");
        }
        let features = settings.features.join(",");
        if !features.is_empty() {
            selection.extend(["--features", &features]);
        }
        run(&format!("wasm library {package}"), &selection)?;
    }
    if args.audio_route == AudioRoute::Include {
        run("wasm FFI observer", consts::WASM_OBSERVER_TEST)?;
    }
    run("wasm DSP layout", consts::WASM_LAYOUT_TEST)?;
    if args.audio_route == AudioRoute::Include {
        run("wasm analysis observers", consts::WASM_ANALYSIS_TEST)?;
    }
    if args.suite == Suite::All {
        run("wasm browser integration", consts::WASM_INTEGRATION_TEST)?;
        if args.audio_route == AudioRoute::Include {
            run("wasm live graph", consts::WASM_LIVE_GRAPH_TEST)?;
        }
    }
    if args.browser == Browser::Chrome {
        run("wasm WebCodecs", consts::WASM_WEBCODECS_TEST)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;
    use kithara_devtools::common::project::ProjectConfig;

    use super::*;
    use crate::ci::process::{Recording, Step};

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: TestArgs,
    }

    #[test]
    fn browser_arguments_keep_the_just_command_surface() {
        let args = TestCli::try_parse_from(["wasm-test"]).unwrap().args;
        assert_eq!(args.browser, Browser::Chrome);
        assert_eq!(args.suite, Suite::All);
        assert_eq!(args.audio_route, AudioRoute::Exclude);
        let args = TestCli::try_parse_from(["wasm-test", "chrome", "webcodecs", "include"])
            .unwrap()
            .args;
        assert_eq!(args.suite, Suite::WebCodecs);
        assert_eq!(args.audio_route, AudioRoute::Include);
        assert!(TestCli::try_parse_from(["wasm-test", "chrome", "unknown"]).is_err());
    }

    fn workspace(settings: &str) -> (tempfile::TempDir, Ctx) {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"declared\", \"defaults\", \"none\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        for (name, metadata) in [
            ("declared", settings),
            (
                "defaults",
                "[package.metadata.kithara.wasm-lib-tests]\ndefault-features = true\nfeatures = []\n",
            ),
            ("none", ""),
        ] {
            let dir = root.path().join(name);
            fs::create_dir_all(dir.join("src")).unwrap();
            fs::write(dir.join("src/lib.rs"), "").unwrap();
            fs::write(
                dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n{metadata}"
                ),
            )
            .unwrap();
        }
        let ctx = Ctx::load_from_manifest(&root.path().join("Cargo.toml")).unwrap();
        (root, ctx)
    }

    /// What `cargo --message-format=json` reports for the two fixture tests,
    /// among lines for their dependencies.
    const FIXTURE_MESSAGES: &str = concat!(
        r#"{"reason":"compiler-artifact","target":{"name":"wasm_bindgen_test","kind":["lib"]},"executable":null}"#,
        "\n",
        r#"{"reason":"compiler-artifact","target":{"name":"browser","kind":["test"]},"executable":"/deps/browser-1.wasm"}"#,
        "\n",
        r#"{"reason":"compiler-artifact","target":{"name":"worker","kind":["test"]},"executable":"/deps/worker-2.wasm"}"#,
        "\n",
        r#"{"reason":"build-finished","success":true}"#,
    );

    #[test]
    fn protocol_fixtures_need_both_modes() {
        let found = fixture_binaries(FIXTURE_MESSAGES).unwrap();
        assert_eq!(found.len(), 2);
        let browser_only = FIXTURE_MESSAGES
            .lines()
            .filter(|line| !line.contains("worker-2"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(fixture_binaries(&browser_only).is_err());
    }

    fn values<'a>(step: &'a Step, flag: &'a str) -> Vec<&'a str> {
        step.args
            .windows(2)
            .filter(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
            .collect()
    }

    /// The runner and its fixture server are built, then the runner's protocol
    /// test runs on both fixture binaries, before any browser suite.
    fn prepares_the_runner(steps: &[Step], target: &Path) {
        let builds = &steps[..2];
        assert_eq!(values(&builds[0], "-p"), ["kithara-integration-tests"]);
        assert_eq!(values(&builds[0], "--bin"), ["test_server"]);
        assert_eq!(values(&builds[1], "-p"), ["kithara-wasm-test-runner"]);
        assert_eq!(values(&builds[1], "--bin"), ["wasm_test_runner"]);
        for build in builds {
            assert_eq!(build.args[0], "build");
            assert!(
                !build
                    .env
                    .contains_key("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER")
            );
        }
        let fixtures = &steps[2];
        assert_eq!(values(fixtures, "-p"), ["kithara-wasm-runner-fixture"]);
        assert_eq!(values(fixtures, "--target"), ["wasm32-unknown-unknown"]);
        assert!(fixtures.args.iter().any(|arg| arg == "--no-run"));
        let protocol = &steps[3];
        assert_eq!(values(protocol, "-p"), ["kithara-wasm-test-runner"]);
        assert_eq!(values(protocol, "--test"), ["protocol"]);
        assert_eq!(
            protocol.env["KITHARA_WASM_RUNNER_FIXTURE_BROWSER"],
            "/deps/browser-1.wasm"
        );
        assert_eq!(
            protocol.env["KITHARA_WASM_RUNNER_FIXTURE_WORKER"],
            "/deps/worker-2.wasm"
        );
        assert_eq!(
            env::split_paths(&protocol.env["PATH"]).next().unwrap(),
            target
        );
    }

    #[test]
    fn browser_suites_preserve_library_closure_runner_order_and_route_selection() {
        let (root, ctx) = workspace(
            "[package.metadata.kithara.wasm-lib-tests]\ndefault-features = false\nfeatures = [\"first\", \"second\"]\n",
        );
        let target = ctx
            .metadata()
            .unwrap()
            .target_directory
            .as_std_path()
            .join("debug");
        fs::remove_file(root.path().join("Cargo.toml")).unwrap();
        for (browser, suite, audio_route) in [
            (Browser::Chrome, Suite::WebCodecs, AudioRoute::Include),
            (Browser::Firefox, Suite::All, AudioRoute::Exclude),
            (Browser::Safari, Suite::All, AudioRoute::Include),
        ] {
            let args = TestArgs {
                browser,
                suite,
                audio_route,
            };
            let process = Process::recording(
                &ctx.root,
                Recording::default()
                    .with_reply(&[ctx.config.tools.program("cargo")], FIXTURE_MESSAGES),
            );
            execute(&args, &ctx, &process).unwrap();
            let recording = process.recorded().unwrap();
            let steps = recording.steps();
            prepares_the_runner(&steps[..4], &target);
            let tests = &steps[4..];
            for step in tests {
                assert_eq!(
                    step.env["CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER"],
                    "wasm_test_runner"
                );
                assert_eq!(step.env["WASM_BINDGEN_TEST_TIMEOUT"], "300");
                assert_eq!(env::split_paths(&step.env["PATH"]).next().unwrap(), target);
                assert_eq!(values(step, "--target"), ["wasm32-unknown-unknown"]);
                assert_eq!(values(step, "--profile"), ["test-release"]);
                assert!(
                    step.args.ends_with(&["--".into(), "--nocapture".into()]),
                    "every browser test streams its console output before the summary: {:?}",
                    step.args
                );
                assert_eq!(
                    values(step, "-Z"),
                    [
                        "build-std=std,panic_abort,core,alloc",
                        "build-std-features=optimize_for_size"
                    ]
                );
            }
            let libs: Vec<_> = tests
                .iter()
                .filter(|step| step.args.iter().any(|arg| arg == "--lib"))
                .collect();
            assert_eq!(libs.len(), 2);
            let declared = libs
                .iter()
                .find(|step| values(step, "-p") == ["declared"])
                .unwrap();
            assert!(
                declared
                    .args
                    .iter()
                    .any(|arg| arg == "--no-default-features")
            );
            assert_eq!(values(declared, "--features"), ["first,second"]);
            let defaults = libs
                .iter()
                .find(|step| values(step, "-p") == ["defaults"])
                .unwrap();
            assert!(
                !defaults
                    .args
                    .iter()
                    .any(|arg| arg == "--no-default-features")
            );
            assert!(values(defaults, "--features").is_empty());
            let has = |test| {
                tests
                    .iter()
                    .any(|step| values(step, "--test").contains(&test))
            };
            assert!(has("session") && has("layout"));
            assert_eq!(has("web-observer"), audio_route == AudioRoute::Include);
            assert_eq!(has("web-bootstrap"), audio_route == AudioRoute::Include);
            assert_eq!(has("web-analysis"), audio_route == AudioRoute::Include);
            assert_eq!(has("ffi_web"), suite == Suite::All);
            assert_eq!(has("suite_heavy"), suite == Suite::All);
            assert_eq!(
                has("suite_live_web_audio"),
                suite == Suite::All && audio_route == AudioRoute::Include
            );
            assert_eq!(has("webcodecs_browser"), browser == Browser::Chrome);
        }
    }

    #[test]
    fn incomplete_library_metadata_refuses_before_any_build_or_test() {
        let (_root, ctx) =
            workspace("[package.metadata.kithara.wasm-lib-tests]\ndefault-features = true\n");
        let process = Process::recording(&ctx.root, Recording::default());
        let args = TestArgs {
            browser: Browser::Chrome,
            suite: Suite::All,
            audio_route: AudioRoute::Exclude,
        };
        let error = execute(&args, &ctx, &process).unwrap_err();
        assert!(format!("{error:#}").contains("declared"));
        assert!(process.recorded().unwrap().steps().is_empty());
    }

    #[test]
    fn unsupported_browser_suite_refuses_before_metadata_or_process_work() {
        let ctx = Ctx::new(std::path::PathBuf::new(), ProjectConfig::default());
        let process = Process::recording(&ctx.root, Recording::default());
        let args = TestArgs {
            browser: Browser::Firefox,
            suite: Suite::WebCodecs,
            audio_route: AudioRoute::Exclude,
        };
        assert!(execute(&args, &ctx, &process).is_err());
        assert!(process.recorded().unwrap().steps().is_empty());
    }
}
