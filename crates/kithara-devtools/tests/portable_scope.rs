//! External Cargo membership, manifest identity, and selected README contracts.

use std::{fs, path::Path, process::Command};

use clap::{CommandFactory, FromArgMatches, Parser};
use kithara_devtools::{CoreCommand, Ctx};
use tempfile::{TempDir, tempdir};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: CoreCommand,
}

fn run(ctx: &Ctx, args: &[&str]) -> anyhow::Result<()> {
    let matches = Cli::command().try_get_matches_from(args)?;
    let cli = Cli::from_arg_matches(&matches)?;
    kithara_devtools::run(&cli.command, ctx)
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture directory");
    fs::write(path, text).expect("write fixture");
}

fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("fixture git")
            .success()
    );
}

fn fixture() -> TempDir {
    let temp = tempdir().expect("fixture directory");
    write(
        temp.path(),
        "Cargo.toml",
        "[package]\nname='lsq-consumer'\nversion='0.1.0'\nedition='2024'\nlicense='MIT'\npublish=false\n\n\
         [workspace]\nmembers=['packages/renamed-directory']\nexclude=['vendor/kithara-external']\n\n\
         [dependencies]\nkithara-external={path='vendor/kithara-external'}\nz-engine={package='lsq-engine',path='packages/renamed-directory'}\n",
    );
    write(
        temp.path(),
        "src/lib.rs",
        "pub struct Root<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    write(
        temp.path(),
        "packages/renamed-directory/Cargo.toml",
        "[package]\nname='lsq-engine'\nversion='0.1.0'\nedition='2024'\nlicense='MIT'\n",
    );
    write(
        temp.path(),
        "packages/renamed-directory/src/lib.rs",
        "pub struct Member<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    write(
        temp.path(),
        "packages/renamed-directory/src/ignored.rs",
        "pub struct Excluded<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    write(
        temp.path(),
        ".config/xtask.toml",
        "[workspace-scan]\nexclude=['packages/renamed-directory/src/ignored.rs']\n",
    );
    write(
        temp.path(),
        "vendor/kithara-external/Cargo.toml",
        "[package]\nname='kithara-external'\nversion='0.1.0'\nedition='2024'\n[workspace]\n",
    );
    write(
        temp.path(),
        "vendor/kithara-external/src/lib.rs",
        "pub struct Vendor<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    temp
}

#[test]
fn cargo_member_names_select_actual_sources_and_leave_vendor_unselected() {
    let temp = fixture();
    let ctx = Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("load consumer");
    let config = temp.path().join(".config/arch");
    let report = temp.path().join("report.md");
    for (selection, path, present, absent) in [
        (
            None,
            None,
            [
                "src/lib.rs::Root",
                "packages/renamed-directory/src/lib.rs::Member",
            ]
            .as_slice(),
            "vendor/kithara-external",
        ),
        (
            Some("lsq-engine"),
            None,
            ["packages/renamed-directory/src/lib.rs::Member"].as_slice(),
            "src/lib.rs::Root",
        ),
        (
            None,
            Some("src/lib.rs"),
            ["src/lib.rs::Root"].as_slice(),
            "packages/renamed-directory/src/lib.rs::Member",
        ),
    ] {
        let mut args = vec![
            "portable",
            "lint",
            "arch",
            "--check",
            "generic_param_count",
            "--config-dir",
            config.to_str().expect("config path"),
            "--report",
            report.to_str().expect("report path"),
        ];
        if let Some(package) = selection {
            args.extend(["--crate", package]);
        }
        if let Some(path) = path {
            args.extend(["--path", path]);
        }
        run(&ctx, &args).expect("the warning-only rule reports selected sources");
        let report = fs::read_to_string(&report).expect("consumer report");
        for expected in present {
            assert!(report.contains(*expected), "{report}");
        }
        assert!(!report.contains(absent), "{report}");
        assert!(!report.contains("Excluded"), "{report}");
    }
}

#[test]
fn root_level_target_keeps_declared_modules_without_scanning_vendor() {
    let temp = fixture();
    let manifest = temp.path().join("Cargo.toml");
    let source = fs::read_to_string(&manifest).expect("fixture manifest");
    write(
        temp.path(),
        "Cargo.toml",
        &format!("{source}\n[lib]\npath='entry.rs'\n"),
    );
    write(
        temp.path(),
        "entry.rs",
        "mod domain;\n#[path=\"shared/renamed.rs\"] mod renamed;\n#[cfg(any())] mod ignored;\n",
    );
    write(temp.path(), "ignored.rs", "invalid Rust {\n");
    write(
        temp.path(),
        ".config/xtask.toml",
        "[workspace-scan]\nexclude=['packages/renamed-directory/src/ignored.rs','ignored.rs']\n",
    );
    write(temp.path(), "domain.rs", "mod nested;\n");
    write(
        temp.path(),
        "domain/nested.rs",
        "pub struct Nested<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    write(
        temp.path(),
        "shared/renamed.rs",
        "pub struct Renamed<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    write(
        temp.path(),
        "orphan.rs",
        "pub struct Orphan<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    let ctx = Ctx::load_from_manifest(&manifest).expect("load consumer");
    run(
        &ctx,
        &[
            "portable",
            "lint",
            "arch",
            "--check",
            "generic_param_count",
            "--crate",
            "lsq-consumer",
            "--report",
            "report.md",
        ],
    )
    .expect("the warning-only rule reports declared modules");
    let report = fs::read_to_string(temp.path().join("report.md")).expect("consumer report");
    assert!(report.contains("domain/nested.rs::Nested"), "{report}");
    assert!(report.contains("shared/renamed.rs::Renamed"), "{report}");
    assert!(!report.contains("vendor/"), "{report}");
    assert!(!report.contains("orphan.rs"), "{report}");
}

#[test]
fn cargo_target_directory_does_not_select_neighbor_packages() {
    let temp = fixture();
    let manifest = temp.path().join("Cargo.toml");
    let source = fs::read_to_string(&manifest).expect("fixture manifest");
    write(
        temp.path(),
        "Cargo.toml",
        &format!("{source}\n[lib]\npath='packages/entry.rs'\n"),
    );
    write(
        temp.path(),
        "packages/entry.rs",
        "pub struct Root<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    write(
        temp.path(),
        "packages/orphan.rs",
        "pub struct Orphan<A,B,C,D,E,F>(A,B,C,D,E,F);\n",
    );
    let ctx = Ctx::load_from_manifest(&manifest).expect("load consumer");
    run(
        &ctx,
        &[
            "portable",
            "lint",
            "arch",
            "--check",
            "generic_param_count",
            "--crate",
            "lsq-consumer",
            "--report",
            "report.md",
        ],
    )
    .expect("selected target has only warning findings");
    let report = fs::read_to_string(temp.path().join("report.md")).expect("consumer report");
    assert!(report.contains("packages/entry.rs::Root"), "{report}");
    for foreign in ["::Member", "::Orphan", "::Vendor"] {
        assert!(!report.contains(foreign), "{report}");
    }
}

#[test]
fn package_text_scope_uses_membership_instead_of_name_substrings() {
    let temp = fixture();
    for path in [
        "packages/other/docs/lsq-engine.md",
        "vendor/lsq-engine-extra/README.md",
    ] {
        write(
            temp.path(),
            path,
            "\u{0414}\u{043E}\u{043A}\u{0443}\u{043C}\u{0435}\u{043D}\u{0442}\n",
        );
    }
    git(temp.path(), &["init", "--quiet"]);
    git(temp.path(), &["add", "."]);
    let ctx = Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("load consumer");
    run(
        &ctx,
        &[
            "portable",
            "lint",
            "style",
            "--check",
            "non_english_text",
            "--crate",
            "lsq-engine",
        ],
    )
    .expect("foreign documents are outside the selected Cargo package");
}

#[test]
fn default_source_roots_preserve_explicit_package_selection() {
    let temp = fixture();
    write(
        temp.path(),
        ".config/xtask.toml",
        "[workspace-scan]\ndefault_roots=['packages/renamed-directory/src']\nexclude=['**/ignored.rs']\n",
    );
    let ctx =
        Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("load selected scan roots");
    for (selection, present, absent) in [
        (
            None,
            "packages/renamed-directory/src/lib.rs::Member",
            "src/lib.rs::Root",
        ),
        (
            Some("lsq-consumer"),
            "src/lib.rs::Root",
            "packages/renamed-directory/src/lib.rs::Member",
        ),
    ] {
        let mut args = vec![
            "portable",
            "lint",
            "arch",
            "--check",
            "generic_param_count",
            "--report",
            "report.md",
        ];
        if let Some(package) = selection {
            args.extend(["--crate", package]);
        }
        run(&ctx, &args).expect("the warning-only rule reports selected sources");
        let report = fs::read_to_string(temp.path().join("report.md")).expect("consumer report");
        assert!(report.contains(present), "{report}");
        assert!(!report.contains(absent), "{report}");
        assert!(!report.contains("Excluded"), "{report}");
    }
}

#[test]
fn generic_deny_fails_and_valid_member_passes() {
    let temp = fixture();
    write(
        temp.path(),
        ".config/arch/thresholds.toml",
        "[file_size]\nwarn=2\ndeny=2\n",
    );
    write(
        temp.path(),
        "packages/renamed-directory/src/lib.rs",
        "pub struct Member;\npub fn play() {}\n",
    );
    let ctx = Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("load consumer");
    let args = [
        "portable",
        "lint",
        "arch",
        "--check",
        "file_size",
        "--crate",
        "lsq-engine",
        "--report",
        "report.md",
    ];
    run(&ctx, &args).expect_err("the generic deny rule rejects its selected source");
    let report = fs::read_to_string(temp.path().join("report.md")).expect("consumer report");
    assert!(
        report.contains("packages/renamed-directory/src/lib.rs"),
        "{report}"
    );
    assert!(!report.contains("vendor/"), "{report}");
    write(
        temp.path(),
        "packages/renamed-directory/src/lib.rs",
        "pub struct Member;\n",
    );
    run(&ctx, &args).expect("the corrected source passes without relaxing the rule");
}

#[test]
fn scoped_denies_keep_their_existing_baseline() {
    let temp = fixture();
    write(
        temp.path(),
        "src/lib.rs",
        "pub fn stop() { Shutdown::root(); }\n",
    );
    write(
        temp.path(),
        ".config/arch/thresholds.toml",
        "[cancel_root_sites]\npatterns=['Shutdown::root']\n",
    );
    write(
        temp.path(),
        ".config/arch/direction.toml",
        "[[layer]]\nname='low'\nindex=0\ncrates=['lsq-consumer']\n\
         [[layer]]\nname='high'\nindex=1\ncrates=['lsq-engine']\n",
    );
    let ctx = Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("load consumer");
    for (check, option, selection, key, foreign) in [
        (
            "direction",
            "--crate",
            "lsq-consumer",
            "lsq-consumer -> lsq-engine",
            "lsq-consumer-extra -> lsq-engine",
        ),
        (
            "cancel_root_sites",
            "--path",
            "src/lib.rs",
            "src/lib.rs:1",
            "src/lib_extra.rs:1",
        ),
    ] {
        write(
            temp.path(),
            ".config/arch/baseline.toml",
            &format!("[{check}]\n'{key}'=1\n'{foreign}'=1\n"),
        );
        run(
            &ctx,
            &[
                "portable",
                "lint",
                "arch",
                "--check",
                check,
                option,
                selection,
                "--report",
                "report.md",
            ],
        )
        .expect("the unchanged selected deny is already baselined");
        let report = fs::read_to_string(temp.path().join("report.md")).expect("selected report");
        assert!(report.contains("- deny violations: 1\n"), "{report}");
        assert!(report.contains(&format!("| DENY | `{key}` |")), "{report}");
        assert!(report.contains("- improvements: 0\n"), "{report}");
        assert!(!report.contains(foreign), "{report}");
    }
}

#[test]
fn manifest_order_uses_member_identity_including_dependency_aliases() {
    let temp = fixture();
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load consumer");
    run(&ctx, &["portable", "manifest", "dependency-order"])
        .expect_err("internal alias after external dependency must fail");
    git(temp.path(), &["init", "--quiet"]);
    git(temp.path(), &["add", "."]);
    git(
        temp.path(),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=.git/hooks",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    run(&ctx, &["portable", "manifest", "dependency-order", "--fix"])
        .expect("normalize target manifests");
    let result = fs::read_to_string(&manifest).expect("read normalized manifest");
    assert!(
        result.find("z-engine=").expect("member alias")
            < result
                .find("kithara-external=")
                .expect("external dependency")
    );
    run(&ctx, &["portable", "manifest", "dependency-order"]).expect("normalized consumer passes");
}

#[test]
fn package_scope_checks_its_readme_outside_the_package_directory() {
    let temp = fixture();
    let member = temp.path().join("packages/renamed-directory/Cargo.toml");
    let source = fs::read_to_string(&member).expect("member manifest");
    fs::write(
        &member,
        format!("{source}publish=false\nreadme='../../shared/docs/../README.md'\n"),
    )
    .expect("declared README");
    fs::create_dir_all(temp.path().join("shared/docs")).expect("README directory");
    let valid = "<img src='https://example.com/logo.svg'>\n\n[License](https://img.shields.io/badge/license-MIT-blue.svg)\n\n# lsq-engine\n\nEngine.\n\n## Usage\n\nUse it.\n";
    write(
        temp.path(),
        "shared/README.md",
        &valid.replace("# lsq-engine", "# wrong"),
    );
    write(
        temp.path(),
        ".config/style/thresholds.toml",
        "[readme_shape]\ninclude_globs=['shared/README.md']\n",
    );
    git(temp.path(), &["init", "--quiet"]);
    git(temp.path(), &["add", "."]);
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("selected README context");
    let args = [
        "portable",
        "lint",
        "style",
        "--check",
        "readme_shape",
        "--crate",
        "lsq-engine",
        "--report",
        "readme-report.md",
    ];
    run(&ctx, &args).expect_err("selected package's external README must be checked");
    let report = fs::read_to_string(temp.path().join("readme-report.md")).expect("README report");
    assert!(
        report.contains("| DENY | `shared/README.md:title` |"),
        "{report}"
    );
    write(
        temp.path(),
        ".config/style/baseline.toml",
        "[readme_shape]\n'shared/README.md:title'=1\n",
    );
    run(&ctx, &args).expect("selected README keeps its existing baseline");
    write(temp.path(), ".config/style/baseline.toml", "");
    write(temp.path(), "shared/README.md", valid);
    run(&ctx, &args).expect("correct README passes");
    let root = fs::read_to_string(&manifest).expect("root manifest");
    fs::write(
        &manifest,
        root.replace("publish=false", "publish=false\nreadme='shared/README.md'"),
    )
    .expect("shared README declaration");
    let ctx = Ctx::load_from_manifest(&manifest).expect("ambiguous README context");
    for package in ["lsq-consumer", "lsq-engine"] {
        let mut args = args;
        args[6] = package;
        let error = run(&ctx, &args).expect_err("every declaring package preserves ambiguity");
        assert!(
            format!("{error:#}").contains("multiple workspace packages"),
            "{error:#}"
        );
    }
}

#[test]
fn readme_license_is_resolved_only_for_selected_packages() {
    let temp = fixture();
    write(
        temp.path(),
        "README.md",
        "<img src='https://example.com/logo.svg'>\n\n[License](https://img.shields.io/badge/license-MIT-blue.svg)\n\n# lsq-consumer\n\nExternal consumer.\n\n## Usage\n\nUse it.\n",
    );
    for args in [["init", "--quiet"].as_slice(), ["add", "."].as_slice()] {
        git(temp.path(), args);
    }
    let config = temp.path().join(".config/style");
    write(
        temp.path(),
        ".config/style/thresholds.toml",
        "[readme_shape]\ninclude_globs=['README.md']\n",
    );
    let ctx = Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("load consumer");
    let args = [
        "portable",
        "lint",
        "style",
        "--check",
        "readme_shape",
        "--config-dir",
        config.to_str().expect("config path"),
    ];
    run(&ctx, &args).expect("selected package's declared MIT license is sufficient");
    let manifest = temp.path().join("Cargo.toml");
    let source = fs::read_to_string(&manifest).expect("fixture manifest");
    fs::write(
        &manifest,
        source.replace("publish=false", "publish=false\nreadme='docs/README.md'"),
    )
    .expect("custom README manifest");
    fs::create_dir(temp.path().join("docs")).expect("README directory");
    fs::rename(
        temp.path().join("README.md"),
        temp.path().join("docs/README.md"),
    )
    .expect("move README");
    git(temp.path(), &["add", "."]);
    write(
        temp.path(),
        ".config/style/thresholds.toml",
        "[readme_shape]\ninclude_globs=['docs/README.md']\n",
    );
    let ctx = Ctx::load_from_manifest(&manifest).expect("load declared README metadata");
    run(&ctx, &args).expect("custom README uses its declaring package");
    write(
        temp.path(),
        ".config/style/thresholds.toml",
        "[readme_shape]\ninclude_globs=['ABSENT.md']\n",
    );
    run(&ctx, &args).expect("no selected README needs no workspace license");
}
