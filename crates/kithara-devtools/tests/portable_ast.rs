//! Caller-owned AST configurations retain their policy after relocation.

use std::{
    fs,
    path::{Path, PathBuf},
};

use kithara_devtools::{CoreCommand, Ctx, ast_grep::AstGrepArgs, run};
use tempfile::{TempDir, tempdir, tempdir_in};

fn fixture(rule: &str, path: &str, source: &str) -> (TempDir, Ctx) {
    let root = tempdir().expect("workspace");
    let library = if path == "lib.rs" {
        path
    } else {
        "custom/lib.rs"
    };
    fs::write(
        root.path().join("Cargo.toml"),
        format!("[package]\nname='lsq-fixture'\nversion='0.1.0'\nedition='2024'\n[lib]\npath='{library}'\n"),
    )
    .expect("manifest");
    fs::create_dir_all(root.path().join("custom")).expect("sources");
    fs::write(root.path().join("custom/lib.rs"), "").expect("library");
    fs::create_dir_all(root.path().join(path).parent().expect("source parent"))
        .expect("source directory");
    fs::write(root.path().join(path), source).expect("source");
    fs::create_dir(root.path().join(".config")).expect("config directory");
    fs::write(
        root.path().join(".config/xtask.toml"),
        "[project]\nname='lsq'\n",
    )
    .expect("policy");
    fs::create_dir(root.path().join("fixture-rules")).expect("caller rule directory");
    let canonical = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.config/ast-grep")
        .join(format!("{rule}.yml"));
    let text = fs::read_to_string(canonical).expect("canonical source");
    let mut definition: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&text).expect("canonical rule");
    definition["files"] = serde_yaml_ng::to_value(["**/*.rs"]).expect("caller source scope");
    fs::write(
        root.path()
            .join("fixture-rules")
            .join(format!("{rule}.yml")),
        serde_yaml_ng::to_string(&definition).expect("derived caller rule"),
    )
    .expect("export canonical rule with caller-owned source scope");
    fs::write(
        root.path().join("sgconfig.yml"),
        "ruleDirs: [fixture-rules]\n",
    )
    .expect("caller scan configuration");
    let ctx = Ctx::load_from_manifest(&root.path().join("Cargo.toml")).expect("context");
    (root, ctx)
}

fn scan(ctx: &Ctx, paths: &[&str], strict: bool) -> anyhow::Result<()> {
    run(
        &CoreCommand::AstGrep(AstGrepArgs {
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            allow_dirty: false,
            fix: false,
            raw: true,
            strict,
        }),
        ctx,
    )
}

#[test]
fn caller_rules_detect_each_violation_in_root_and_custom_sources() {
    for (rule, source) in [
        (
            "rust.no-debug-prints",
            "fn production() { println!(\"debug\"); }",
        ),
        (
            "rust.no-inherent-to-string",
            "struct Value; impl Value { fn to_string(&self) -> String { String::new() } }",
        ),
        (
            "perf.no-collect-iter-roundtrip",
            "fn production() { let _ = (0..3).collect::<Vec<_>>().into_iter(); }",
        ),
    ] {
        for path in ["lib.rs", "custom/lib.rs"] {
            let (root, ctx) = fixture(rule, path, source);
            let cwd = std::env::current_dir().expect("caller cwd");
            assert_ne!(cwd, root.path());
            assert!(scan(&ctx, &[], true).is_err(), "{rule} violation in {path}");
            fs::write(root.path().join(path), "pub struct Value;").expect("corrected");
            scan(&ctx, &[], true).expect("corrected source");
            assert_eq!(std::env::current_dir().expect("caller cwd"), cwd);
            assert!(root.path().join("sgconfig.yml").is_file());
        }
    }
}

#[test]
fn standalone_relative_root_resolves_its_native_configuration_once() {
    let cwd = std::env::current_dir().expect("caller cwd");
    let root = tempdir_in(&cwd).expect("relative-root workspace");
    fs::create_dir(root.path().join("rules")).expect("caller rules");
    fs::write(root.path().join("sgconfig.yml"), "ruleDirs: [rules]\n")
        .expect("native configuration");
    fs::write(
        root.path().join("rules/value.yml"),
        "id: caller.value\nlanguage: Rust\nseverity: error\nmessage: value\nrule: {kind: integer_literal, regex: '^42$'}\n",
    )
    .expect("caller rule");
    let source = root.path().join("value.rs");
    fs::write(&source, "fn value() -> u32 { 42 }").expect("violation");
    let relative = PathBuf::from(root.path().file_name().expect("relative component"));
    let ctx = Ctx::new(
        relative,
        kithara_devtools::common::project::ProjectConfig::default(),
    );
    let error = scan(&ctx, &[], true).expect_err("relative-root finding");
    assert!(
        error
            .downcast_ref::<kithara_devtools::verdict::NotClean>()
            .is_some(),
        "{error:#}"
    );
    fs::write(source, "pub struct Value;").expect("corrected source");
    scan(&ctx, &[], true).expect("relative root keeps its native configuration");
}

#[test]
fn warning_promotion_and_unselected_project_policy_remain_explicit() {
    let (root, ctx) = fixture(
        "rust.no-debug-prints",
        "custom/lib.rs",
        "fn production() { println!(\"debug\"); }",
    );
    scan(&ctx, &[], false).expect("ordinary warning");
    assert!(scan(&ctx, &[], true).is_err());
    fs::write(
        root.path().join("custom/lib.rs"),
        "#[test] fn stock_test() {} fn production() { let _ = std::sync::Arc::new(1); }",
    )
    .expect("unselected policy");
    scan(&ctx, &[], true).expect("no implicit Kithara ownership");
}

#[test]
fn malformed_caller_rule_catalog_is_rejected() {
    let (root, ctx) = fixture("rust.no-debug-prints", "custom/lib.rs", "");
    fs::write(
        root.path().join("sgconfig.yml"),
        "ruleDirs: [missing-rules]\n",
    )
    .expect("missing caller directory");
    assert!(scan(&ctx, &[], true).is_err(), "missing rule directory");
    fs::write(
        root.path().join("sgconfig.yml"),
        "ruleDirs: [fixture-rules]\n",
    )
    .expect("caller directory");
    fs::copy(
        root.path().join("fixture-rules/rust.no-debug-prints.yml"),
        root.path().join("fixture-rules/duplicate.yml"),
    )
    .expect("duplicate caller identity");
    let error = scan(&ctx, &[], true).expect_err("duplicate identity");
    assert!(error.to_string().contains("duplicate ast-grep rule"));
}

#[test]
fn explicit_paths_override_declared_sources() {
    let (root, ctx) = fixture("rust.no-debug-prints", "custom/lib.rs", "pub struct Value;");
    let path = Path::new("other/fixture.rs");
    fs::create_dir(root.path().join("other")).expect("other directory");
    fs::write(
        root.path().join(path),
        "fn production() { println!(\"debug\"); }",
    )
    .expect("other source");
    scan(&ctx, &[], true).expect("out of default scope");
    assert!(scan(&ctx, &["other/fixture.rs"], true).is_err());
}

#[test]
fn scan_all_bypasses_caller_exclusions_and_preserves_rule_ignores() {
    let (root, mut ctx) = fixture(
        "rust.no-debug-prints",
        "custom/lib.rs",
        "#[cfg(test)] fn fixture() { println!(\"debug\"); }",
    );
    scan(&ctx, &[], true).expect("inline test is excluded");
    ctx.config.lint_exclude.scan_all_rules = vec!["rust.no-debug-prints".to_owned()];
    assert!(scan(&ctx, &[], true).is_err());
    fs::write(root.path().join("custom/lib.rs"), "mod fixture;").expect("external source");
    fs::write(
        root.path().join("custom/fixture.rs"),
        "fn fixture() { println!(\"debug\"); }",
    )
    .expect("external test source");
    ctx.config.lint_exclude.scan_all_rules.clear();
    ctx.config.lint_exclude.paths = vec!["**/fixture.rs".to_owned()];
    scan(&ctx, &[], true).expect("test path is excluded");
    ctx.config.lint_exclude.scan_all_rules = vec!["rust.no-debug-prints".to_owned()];
    assert!(scan(&ctx, &[], true).is_err());
    fs::write(root.path().join("custom/fixture.rs"), "pub struct Value;")
        .expect("corrected external source");
    fs::write(root.path().join("custom/lib.rs"), "mod fixture_tests;")
        .expect("native ignored source");
    fs::write(
        root.path().join("custom/fixture_tests.rs"),
        "fn fixture() { println!(\"debug\"); }",
    )
    .expect("native ignored test source");
    scan(&ctx, &[], true).expect("scan-all preserves the canonical rule's native ignores");
}

#[test]
fn caller_rule_directories_compose_without_duplicate_identities() {
    let (root, _) = fixture("rust.no-debug-prints", "custom/lib.rs", "pub struct Value;");
    fs::create_dir(root.path().join("local-rules")).expect("local directory");
    fs::write(
        root.path().join("sgconfig.yml"),
        "ruleDirs: [fixture-rules, local-rules]\n",
    )
    .expect("local config");
    let rule = root.path().join("local-rules/value.yml");
    fs::write(
        &rule,
        "id: local.value\nlanguage: Rust\nseverity: error\nmessage: local value\nrule:\n  kind: type_identifier\n  regex: '^Value$'\n",
    )
    .expect("local rule");
    let ctx = Ctx::load_from_manifest(&root.path().join("Cargo.toml")).expect("context");
    assert!(scan(&ctx, &[], true).is_err(), "local finding");
    fs::write(root.path().join("custom/lib.rs"), "pub struct Correct;").expect("corrected");
    scan(&ctx, &[], true).expect("composed corrected fixture");
    fs::write(
        &rule,
        "id: rust.no-debug-prints\nlanguage: Rust\nseverity: error\nmessage: duplicate\nrule:\n  kind: type_identifier\n",
    )
    .expect("duplicate identity");
    let error = scan(&ctx, &[], true).expect_err("caller identity is ambiguous");
    assert!(error.to_string().contains("duplicate ast-grep rule"));
}

#[test]
fn executable_failure_and_foreign_fix_root_are_errors() {
    let (root, mut ctx) = fixture("rust.no-debug-prints", "custom/lib.rs", "pub struct Value;");
    let tool = root.path().join("missing-ast-grep");
    let policy = format!(
        "[tools.ast-grep]\nprogram={}\n",
        serde_json::to_string(&tool).expect("tool path"),
    );
    fs::write(root.path().join(".config/xtask.toml"), policy).expect("tool role");
    ctx.config =
        kithara_devtools::common::project::ProjectConfig::load(root.path()).expect("tool config");
    assert!(scan(&ctx, &[], true).is_err(), "missing executable");
    let error = run(
        &CoreCommand::AstGrep(AstGrepArgs {
            paths: Vec::new(),
            allow_dirty: true,
            fix: true,
            raw: true,
            strict: true,
        }),
        &ctx,
    )
    .expect_err("foreign root must not be fixed");
    assert!(error.to_string().contains("workspace root"));
    assert_eq!(
        fs::read_to_string(root.path().join("custom/lib.rs")).expect("unchanged source"),
        "pub struct Value;",
    );
}

#[test]
fn scan_all_keeps_other_production_rules_in_the_same_report_category() {
    let (root, mut ctx) = fixture("rust.no-debug-prints", "custom/lib.rs", "pub struct Value;");
    fs::create_dir(root.path().join("local-rules")).expect("local directory");
    fs::write(
        root.path().join("sgconfig.yml"),
        "ruleDirs: [fixture-rules, local-rules]\n",
    )
    .expect("local config");
    fs::write(
        root.path().join("local-rules/values.yml"),
        "id: values.one\nlanguage: Rust\nseverity: error\nmessage: value one\nmetadata: {category: values}\nrule: {kind: integer_literal, regex: '^1$'}\n---\nid: values.two\nlanguage: Rust\nseverity: error\nmessage: value two\nmetadata: {category: values}\nrule: {kind: integer_literal, regex: '^2$'}\n",
    )
    .expect("two rules");
    fs::write(
        root.path().join("custom/lib.rs"),
        "pub fn production() -> i32 { 2 } #[cfg(test)] fn fixture() -> i32 { 1 }",
    )
    .expect("production and test violations");
    ctx.config.lint_exclude.scan_all_rules = vec!["values.one".to_owned()];
    let error = scan(&ctx, &[], true).expect_err("both findings remain");
    let verdict = error
        .downcast_ref::<kithara_devtools::verdict::NotClean>()
        .expect("lint verdict");
    assert_eq!(verdict.findings, Some(2));
}

#[test]
fn custom_layout_test_modules_are_excluded_until_production_reuses_them() {
    let (root, mut ctx) = fixture(
        "rust.no-debug-prints",
        "custom/lib.rs",
        "#[cfg(test)] mod fixture;",
    );
    fs::write(
        root.path().join("custom/fixture.rs"),
        "fn fixture() { println!(\"debug\"); }",
    )
    .expect("test module");
    scan(&ctx, &[], true).expect("test-only declared module");
    fs::write(
        root.path().join("custom/lib.rs"),
        "#[cfg(test)] mod fixture; #[path=\"fixture.rs\"] mod production;",
    )
    .expect("production reuse");
    assert!(
        scan(&ctx, &[], true).is_err(),
        "production reuse stays checked"
    );
    fs::write(
        root.path().join("custom/lib.rs"),
        "#[cfg(test)] mod fixture;",
    )
    .expect("test-only declaration");
    ctx.config.lint_exclude.scan_all_rules = vec!["rust.no-debug-prints".to_owned()];
    assert!(
        scan(&ctx, &[], true).is_err(),
        "scan-all still checks the module"
    );
}

#[test]
fn project_ast_scope_keeps_workspace_manifest_and_scan_all_tests() {
    let (root, _) = fixture("rust.no-debug-prints", "custom/lib.rs", "pub struct Voice;");
    fs::create_dir(root.path().join("crates")).expect("Rust default domain");
    fs::write(
        root.path().join(".config/xtask.toml"),
        "[ast-grep]\ndefault_roots=['.']\n[workspace-scan]\ndefault_roots=['crates']\n[lint_exclude]\npaths=['tests/**']\nscan_all_rules=['local.value']\n",
    )
    .expect("separate AST source domain");
    fs::create_dir(root.path().join("local-rules")).expect("local rules");
    fs::write(
        root.path().join("sgconfig.yml"),
        "ruleDirs: [fixture-rules, local-rules]\nlanguageGlobs:\n  Yaml: ['**/*.toml']\n",
    )
    .expect("local config");
    fs::write(
        root.path().join("local-rules/value.yml"),
        "id: local.value\nlanguage: Rust\nseverity: error\nmessage: test value\nrule: {kind: integer_literal, regex: '^42$'}\n",
    )
    .expect("scan-all rule");
    fs::write(
        root.path().join("local-rules/manifest.yml"),
        "id: local.manifest\nlanguage: Yaml\nseverity: error\nmessage: manifest comment\nrule: {kind: comment, regex: '^# BAD'}\n",
    )
    .expect("manifest rule");
    let manifest = root.path().join("Cargo.toml");
    let text = fs::read_to_string(&manifest).expect("manifest");
    fs::write(&manifest, format!("{text}# BAD\n")).expect("manifest finding");
    fs::create_dir(root.path().join("tests")).expect("top-level tests");
    fs::write(
        root.path().join("tests/fixture.rs"),
        "fn fixture() -> u32 { 42 }",
    )
    .expect("top-level test source");
    let ctx = Ctx::load_from_manifest(&manifest).expect("context");
    let error = scan(&ctx, &[], true).expect_err("both existing AST domains stay enforced");
    let verdict = error
        .downcast_ref::<kithara_devtools::verdict::NotClean>()
        .expect("lint verdict");
    assert_eq!(verdict.findings, Some(2));
}

#[test]
fn local_utility_directories_work_in_the_scan_all_catalog() {
    let (root, mut ctx) = fixture(
        "rust.no-debug-prints",
        "custom/lib.rs",
        "#[cfg(test)] mod fixture;",
    );
    for directory in ["local-rules", "local-utils"] {
        fs::create_dir(root.path().join(directory)).expect("local rule directory");
    }
    fs::write(
        root.path().join("custom/fixture.rs"),
        "fn fixture() -> u32 { 42 }",
    )
    .expect("test source");
    fs::write(
        root.path().join("sgconfig.yml"),
        "ruleDirs: [fixture-rules, local-rules]\nutilDirs: [local-utils]\n",
    )
    .expect("local utility configuration");
    fs::write(
        root.path().join("local-utils/literal.yml"),
        "id: forty-two\nlanguage: Rust\nrule: {kind: integer_literal, regex: '^42$'}\n",
    )
    .expect("local utility");
    fs::write(
        root.path().join("local-rules/value.yml"),
        "id: local.value\nlanguage: Rust\nseverity: error\nmessage: local value\nrule: {matches: forty-two}\n",
    )
    .expect("utility consumer");
    scan(&ctx, &[], true).expect("production scan excludes the test source");
    ctx.config.lint_exclude.scan_all_rules = vec!["local.value".to_owned()];
    let error = scan(&ctx, &[], true).expect_err("scan-all resolves the local utility");
    assert_eq!(
        error
            .downcast_ref::<kithara_devtools::verdict::NotClean>()
            .expect("lint verdict")
            .findings,
        Some(1)
    );
    fs::write(
        root.path().join("custom/fixture.rs"),
        "fn fixture() -> u32 { 43 }",
    )
    .expect("corrected source");
    scan(&ctx, &[], true).expect("corrected utility consumer");
}

#[test]
fn non_diagnostic_tool_exit_is_not_a_clean_scan() {
    let (root, _) = fixture("rust.no-debug-prints", "custom/lib.rs", "pub struct Voice;");
    fs::write(
        root.path().join(".config/xtask.toml"),
        "[tools.ast-grep]\nprogram='rustc'\n",
    )
    .expect("non-scanner tool role");
    let ctx = Ctx::load_from_manifest(&root.path().join("Cargo.toml")).expect("context");
    let error = scan(&ctx, &[], true).expect_err("tool failure cannot pass");
    assert!(error.to_string().contains("ast-grep failed"));
}

#[test]
fn path_arguments_keep_their_literal_scope() {
    let (root, ctx) = fixture(
        "rust.no-debug-prints",
        "--update-all/fixture.rs",
        "fn value() -> u32 { 42 }",
    );
    fs::create_dir(root.path().join("local-rules")).expect("local rules");
    fs::write(
        root.path().join("sgconfig.yml"),
        "ruleDirs: [fixture-rules, local-rules]\n",
    )
    .expect("local configuration");
    fs::write(
        root.path().join("local-rules/value.yml"),
        "id: local.value\nlanguage: Rust\nseverity: error\nmessage: value\nrule: {kind: integer_literal, regex: '^42$'}\nfix: '43'\n",
    )
    .expect("local fix rule");
    fs::write(
        root.path().join("custom/lib.rs"),
        "pub fn outside() -> u32 { 42 }",
    )
    .expect("out-of-scope source");
    let error = scan(&ctx, &["--update-all"], true).expect_err("a check reports the finding");
    assert_eq!(
        error
            .downcast_ref::<kithara_devtools::verdict::NotClean>()
            .unwrap_or_else(|| panic!("lint verdict: {error:#}"))
            .findings,
        Some(1)
    );
    assert_eq!(
        fs::read_to_string(root.path().join("--update-all/fixture.rs")).expect("unchanged source"),
        "fn value() -> u32 { 42 }",
    );
}
