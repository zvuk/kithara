use std::{collections::BTreeMap, fs, path::Path, process::Command};

use super::{
    command::{NextestAction, requested_lane, run_draft},
    inventory::complete_inventory,
    package_build::admit,
    request::TestRequest,
    resolve::ResolvedLane,
    unit_graph::UnitGraph,
};
use crate::{Ctx, test::draft::DraftScope};

fn workspace(optional: &str) -> (tempfile::TempDir, Ctx) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir(root.join(".config")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = ['required', 'optional']\nresolver = '3'\n",
    )
    .unwrap();
    for package in ["required", "optional"] {
        fs::create_dir_all(root.join(package).join("src")).unwrap();
        fs::write(
            root.join(package).join("Cargo.toml"),
            format!("[package]\nname = '{package}'\nversion = '0.1.0'\nedition = '2024'\n"),
        )
        .unwrap();
    }
    fs::write(
        root.join("required/src/lib.rs"),
        "#[test]\nfn required_contract_executes() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();
    fs::write(root.join("optional/src/lib.rs"), optional).unwrap();
    fs::write(
        root.join(".config/xtask.toml"),
        "[test]\ndefault_lane = 'workspace'\ndefault_backend = 'none'\nnextest_config = '.config/nextest.toml'\n[test.required]\npackages = ['required']\n[test.flash]\ndefault = false\n[test.net_backends.none]\n[test.lanes.workspace.cargo]\nworkspace = true\n",
    )
    .unwrap();
    fs::write(
        root.join(".config/nextest.toml"),
        "[profile.default]\nretries = 0\n[profile.default.junit]\npath = 'junit.xml'\n",
    )
    .unwrap();
    let mut ctx = Ctx::load_from_manifest(&root.join("Cargo.toml")).unwrap();
    let target = root.join("target");
    ctx.config
        .test
        .lanes
        .get_mut("workspace")
        .unwrap()
        .env
        .insert("CARGO_TARGET_DIR".to_owned(), target.display().to_string());
    let pins: toml::Table =
        fs::read_to_string(super::repository_tests::root().join(".config/ci-pins.toml"))
            .unwrap()
            .parse()
            .unwrap();
    let env = &mut ctx.config.test.lanes.get_mut("workspace").unwrap().env;
    let cargo_home = root.join("cargo-home");
    fs::create_dir(&cargo_home).unwrap();
    env.insert("CARGO_HOME".to_owned(), cargo_home.display().to_string());
    env.insert("NEXTEST_PROFILE".to_owned(), "default".to_owned());
    for (name, key) in [
        ("RUSTUP_TOOLCHAIN", "stable_toolchain"),
        ("KITHARA_NIGHTLY_TOOLCHAIN", "nightly_toolchain"),
    ] {
        env.insert(name.to_owned(), pins[key].as_str().unwrap().to_owned());
    }
    let setup = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .envs(&ctx.config.test.lanes["workspace"].env)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    (temporary, ctx)
}

#[test]
fn a_package_only_draft_runs_its_test_without_compiling_an_optional_broken_package() {
    let (temporary, ctx) =
        workspace("compile_error!(\"the optional package must not be compiled\");\n");
    let root = temporary.path();
    let target = root.join("target");
    let test = &ctx.config.test;
    let baseline = Command::new("cargo")
        .args(["test", "--workspace", "--no-run", "--offline"])
        .envs(&test.lanes["workspace"].env)
        .env("CARGO_TARGET_DIR", &target)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!baseline.status.success());
    assert!(
        String::from_utf8_lossy(&baseline.stderr)
            .contains("the optional package must not be compiled")
    );
    let request = TestRequest::parse(&["--draft".to_owned()]).unwrap();
    let scope = DraftScope::load(test, root, None).unwrap();
    run_draft(test, root, "workspace", &scope, &request)
        .expect("a proved package-only Draft must not build the optional package");
    let junit = fs::read_to_string(target.join("nextest/default/junit.xml")).unwrap();
    let cases = crate::junit::parse_junit(&junit).unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].name, "required_contract_executes");
    assert!(!cases[0].failed);
}

fn inventory(lane: &ResolvedLane, root: &Path) -> BTreeMap<String, BTreeMap<String, bool>> {
    let args = [
        "--message-format=json",
        "--ignore-default-filter",
        "--run-ignored=all",
    ]
    .map(str::to_owned);
    let output = lane
        .command(NextestAction::List, &args)
        .unwrap()
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    complete_inventory(&String::from_utf8(output.stdout).unwrap()).unwrap()
}

#[test]
fn native_graphs_artifacts_and_generated_selections_agree_for_admitted_profiles() {
    let (temporary, mut ctx) = workspace("");
    let root = temporary.path();
    fs::write(root.join("required/src/lib.rs"),
        "#[test]\nfn required_contract_executes() {}\n#[test]\n#[ignore]\nfn ignored_contract_stays_ignored() { panic!(\"ignored\"); }\n").unwrap();
    fs::write(root.join("Cargo.toml"),
        "[workspace]\nmembers = ['required', 'optional', 'inactive']\nresolver = '3'\n[profile.test-release]\ninherits = 'release'\ndebug-assertions = true\noverflow-checks = true\n").unwrap();
    fs::create_dir_all(root.join("inactive/tests")).unwrap();
    fs::write(root.join("inactive/Cargo.toml"),
        "[package]\nname = 'inactive'\nversion = '0.1.0'\nedition = '2024'\nautotests = false\n[features]\nenabled = []\n[[test]]\nname = 'gated'\npath = 'tests/gated.rs'\nrequired-features = ['enabled']\n").unwrap();
    fs::write(
        root.join("inactive/tests/gated.rs"),
        "#[test]\nfn inactive_contract() {}\n",
    )
    .unwrap();
    ctx.config
        .test
        .required
        .packages
        .push("inactive".to_owned());
    let setup = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .envs(&ctx.config.test.lanes["workspace"].env)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let request = TestRequest::parse(&["--draft".to_owned()]).unwrap();
    let pin = ctx.config.test.lanes["workspace"].env["KITHARA_NIGHTLY_TOOLCHAIN"].clone();
    let located = Command::new("rustup")
        .args(["which", "--toolchain", &pin, "cargo"])
        .output()
        .unwrap();
    assert!(located.status.success());
    let cargo = String::from_utf8(located.stdout).unwrap();
    for profile in ["test", "test-release"] {
        ctx.config
            .test
            .lanes
            .get_mut("workspace")
            .unwrap()
            .cargo
            .profile = Some(profile.to_owned());
        let test = &ctx.config.test;
        let scope = DraftScope::load(test, root, None).unwrap();
        let full = requested_lane(test, "workspace", &request).unwrap();
        let mut before = inventory(&full, root);
        before.retain(|binary, _| binary == "required" || binary.starts_with("required::"));
        let native = Command::new("cargo")
            .args([
                "test",
                "--workspace",
                "--no-run",
                "--offline",
                "--locked",
                "--message-format=json",
                "--profile",
                profile,
            ])
            .envs(&full.env)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            native.status.success(),
            "{}",
            String::from_utf8_lossy(&native.stderr)
        );
        let graph = Command::new(cargo.trim())
            .args([
                "test",
                "--workspace",
                "--no-run",
                "--offline",
                "--locked",
                "--unit-graph",
                "-Z",
                "unstable-options",
                "--profile",
                profile,
            ])
            .envs(&full.env)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            graph.status.success(),
            "{}",
            String::from_utf8_lossy(&graph.stderr)
        );
        UnitGraph::parse(&graph.stdout)
            .unwrap()
            .verify_artifacts(&native.stdout)
            .unwrap();
        let candidate = admit(full, &scope, &request, root).unwrap();
        assert!(!candidate.cargo.workspace);
        assert_eq!(candidate.cargo.packages, ["inactive", "required"]);
        let after = inventory(&candidate, root);
        assert_eq!(before, after);
        assert_eq!(after.values().map(BTreeMap::len).sum::<usize>(), 2);
        assert!(
            after
                .values()
                .flat_map(BTreeMap::values)
                .any(|ignored| *ignored)
        );
    }
}

#[test]
fn feature_unification_mismatch_and_arbitrary_filters_keep_the_full_build() {
    let (temporary, ctx) = workspace("");
    let root = temporary.path();
    fs::write(root.join("required/Cargo.toml"),
        "[package]\nname = 'required'\nversion = '0.1.0'\nedition = '2024'\n[features]\nextra = []\n").unwrap();
    fs::write(root.join("optional/Cargo.toml"),
        "[package]\nname = 'optional'\nversion = '0.1.0'\nedition = '2024'\n[dependencies]\nrequired = { path = '../required', features = ['extra'] }\n").unwrap();
    let setup = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(setup.status.success());
    let test = &ctx.config.test;
    let request = TestRequest::parse(&["--draft".to_owned()]).unwrap();
    for block in [None, Some("filter = 'test(required_contract_executes)'")] {
        let scope = DraftScope::load(test, root, block).unwrap();
        let full = requested_lane(test, "workspace", &request).unwrap();
        let original = full.command(NextestAction::Run, &[]).unwrap();
        let admitted = admit(full, &scope, &request, root).unwrap();
        let command = admitted.command(NextestAction::Run, &[]).unwrap();
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            original.get_args().collect::<Vec<_>>()
        );
        assert_eq!(
            command.get_envs().collect::<Vec<_>>(),
            original.get_envs().collect::<Vec<_>>()
        );
        assert!(admitted.cargo.workspace);
    }
}

#[test]
fn a_stale_lock_fails_graph_resolution_without_rewriting_it() {
    let (temporary, ctx) = workspace("");
    let root = temporary.path();
    let lock = fs::read(root.join("Cargo.lock")).unwrap();
    fs::write(
        root.join("optional/Cargo.toml"),
        "[package]\nname = 'optional'\nversion = '0.2.0'\nedition = '2024'\n",
    )
    .unwrap();
    let request = TestRequest::parse(&["--draft".to_owned()]).unwrap();
    let scope = DraftScope::load(&ctx.config.test, root, None).unwrap();
    let full = requested_lane(&ctx.config.test, "workspace", &request).unwrap();
    let error = admit(full, &scope, &request, root).unwrap_err();
    assert!(format!("{error:#}").contains("--locked"));
    assert_eq!(lock, fs::read(root.join("Cargo.lock")).unwrap());
}

#[test]
fn removing_a_primary_dependency_keeps_its_full_build_environment() {
    let (temporary, ctx) = workspace("");
    let root = temporary.path();
    let test = &ctx.config.test;
    fs::write(root.join("required/Cargo.toml"),
        "[package]\nname = 'required'\nversion = '0.1.0'\nedition = '2024'\n[dependencies]\noptional = { path = '../optional' }\n").unwrap();
    fs::write(
        root.join("optional/src/lib.rs"),
        "pub const PRIMARY: bool = option_env!(\"CARGO_PRIMARY_PACKAGE\").is_some();\n",
    )
    .unwrap();
    fs::write(
        root.join("required/src/lib.rs"),
        "#[test]\nfn required_environment_matches_main() { assert!(optional::PRIMARY); }\n",
    )
    .unwrap();
    let baseline = Command::new("cargo")
        .args(["test", "--workspace", "--offline"])
        .envs(&test.lanes["workspace"].env)
        .env("CARGO_TARGET_DIR", root.join("baseline-target"))
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let request = TestRequest::parse(&["--draft".to_owned()]).unwrap();
    let scope = DraftScope::load(test, root, None).unwrap();
    let full = requested_lane(test, "workspace", &request).unwrap();
    let candidate = admit(full, &scope, &request, root).unwrap();
    assert!(
        candidate.cargo.workspace,
        "CARGO_PRIMARY_PACKAGE must remain equal to the full build"
    );
    run_draft(test, root, "workspace", &scope, &request).unwrap();
}

#[test]
fn losing_every_feature_gated_root_retains_the_full_build() {
    let (temporary, ctx) = workspace("");
    let root = temporary.path();
    let test = &ctx.config.test;
    fs::write(root.join("required/Cargo.toml"),
        "[package]\nname = 'required'\nversion = '0.1.0'\nedition = '2024'\n[lib]\ntest = false\ndoctest = false\n[features]\nenabled = []\n[[test]]\nname = 'gated'\npath = 'tests/gated.rs'\nrequired-features = ['enabled']\n").unwrap();
    fs::create_dir(root.join("required/tests")).unwrap();
    fs::write(
        root.join("required/tests/gated.rs"),
        "#[test]\nfn gated_contract() {}\n",
    )
    .unwrap();
    fs::write(root.join("optional/Cargo.toml"),
        "[package]\nname = 'optional'\nversion = '0.1.0'\nedition = '2024'\n[dependencies]\nrequired = { path = '../required', features = ['enabled'] }\n").unwrap();
    let setup = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .envs(&test.lanes["workspace"].env)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(setup.status.success());
    let request = TestRequest::parse(&["--draft".to_owned()]).unwrap();
    let scope = DraftScope::load(test, root, None).unwrap();
    let full = requested_lane(test, "workspace", &request).unwrap();
    let candidate = admit(full, &scope, &request, root).unwrap();
    assert!(candidate.cargo.workspace);
    run_draft(test, root, "workspace", &scope, &request).unwrap();
    let junit = fs::read_to_string(root.join("target/nextest/default/junit.xml")).unwrap();
    assert!(
        crate::junit::parse_junit(&junit)
            .unwrap()
            .iter()
            .any(|case| case.name == "gated_contract" && !case.failed)
    );
}
