use std::{fs, path::Path, process::Command};

use serde_json::{Value, json};

use super::compare;

type TestCases<'a> = &'a [(&'a str, bool)];

fn inventory(binaries: &[(&str, TestCases<'_>)]) -> String {
    let mut suites = serde_json::Map::new();
    for (binary, tests) in binaries {
        let tests = tests
            .iter()
            .map(|(name, ignored)| {
                (
                    (*name).to_owned(),
                    json!({"ignored": ignored, "filter-match": {"status": "matches"}}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        suites.insert(
            (*binary).to_owned(),
            json!({"binary-id": binary, "status": "listed", "testcases": tests}),
        );
    }
    json!({"rust-suites": suites}).to_string()
}

#[test]
fn a_binary_can_split_without_a_per_test_rename_table() {
    let before = inventory(&[("old::suite", &[("a::one", false), ("b::two", true)])]);
    let after = inventory(&[
        ("new-a::suite", &[("a::one", false)]),
        ("new-b::suite", &[("b::two", true)]),
    ]);
    let renames = "[binaries]\n'old::suite' = ['new-a::suite', 'new-b::suite']";
    assert_eq!(compare(&before, &after, renames).unwrap(), 2);
}

#[test]
fn dropped_added_and_relabelled_tests_are_reported() {
    let before = inventory(&[("demo::suite", &[("gone", false), ("changed", true)])]);
    let after = inventory(&[("demo::suite", &[("extra", false), ("changed", false)])]);
    let error = compare(&before, &after, "[binaries]").unwrap_err();
    assert_eq!(
        error.to_string(),
        "test census differs: base 2, candidate 2\nmissing (1):\ndemo::suite\tgone\nadded (1):\ndemo::suite\textra\nchanged ignored flag (1):\ndemo::suite\tchanged"
    );
}

#[test]
fn duplicated_tests_cannot_hide_in_two_successors() {
    let before = inventory(&[("old", &[("one", false)])]);
    let after = inventory(&[("left", &[("one", false)]), ("right", &[("one", false)])]);
    let error = compare(&before, &after, "[binaries]\nold = ['left', 'right']").unwrap_err();
    assert_eq!(
        error.to_string(),
        "census duplicates `old\tone` across successor binaries"
    );
}

#[test]
fn rename_tables_reject_missing_or_ambiguous_binaries() {
    let before = inventory(&[("old", &[("one", false)])]);
    let after = inventory(&[("new", &[("one", false)])]);
    for table in [
        "[binaries]\nmissing = ['new']",
        "[binaries]\nold = ['missing']",
        "[binaries]\nold = []",
        "[binaries]\nold = ['new', 'new']",
        "unknown = true\n[binaries]\nold = ['new']",
    ] {
        assert!(compare(&before, &after, table).is_err(), "{table}");
    }
}

#[test]
fn filtered_or_empty_inventories_cannot_claim_preservation() {
    let original = inventory(&[("demo", &[("one", false)])]);
    let mut filtered: Value = serde_json::from_str(&original).unwrap();
    filtered["rust-suites"]["demo"]["testcases"]["one"]["filter-match"]["status"] =
        json!("mismatch");
    assert!(compare(&original, &filtered.to_string(), "[binaries]").is_err());
    let mut skipped: Value = serde_json::from_str(&original).unwrap();
    skipped["rust-suites"]["demo"]["status"] = json!("skipped-default-filter");
    assert!(compare(&skipped.to_string(), &original, "[binaries]").is_err());
    let empty = inventory(&[]);
    assert!(compare(&empty, &empty, "[binaries]").is_err());
}

fn list(root: &Path) -> String {
    let cargo = std::env::var_os("CARGO").expect("cargo supplied by the test runner");
    let output = Command::new(cargo)
        .current_dir(root)
        .args([
            "nextest",
            "list",
            "--offline",
            "--ignore-default-filter",
            "--run-ignored=all",
            "--message-format=json",
        ])
        .env_remove("NEXTEST_PROFILE")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env("CARGO_TARGET_DIR", root.join("build"))
        .env("CARGO_HOME", root.join("cargo-home"))
        .output()
        .expect("native nextest inventory");
    assert!(
        output.status.success(),
        "nextest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("nextest JSON")
}

fn package(root: &Path, name: &str, suite: &str) {
    let package = root.join(name);
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("Cargo.toml"),
        format!(
            "[package]\nname = '{name}'\nversion = '0.0.0'\nedition = '2024'\n[[test]]\nname = 'suite'\npath = 'suite.rs'\n"
        ),
    )
    .unwrap();
    fs::write(package.join("suite.rs"), suite).unwrap();
}

#[test]
fn native_nextest_accepts_a_moved_file_and_detects_its_removal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = ['original']\nresolver = '2'\n",
    )
    .unwrap();
    package(root, "original", "mod kept;\nmod moved;\n");
    fs::write(root.join("original/kept.rs"), "#[test]\nfn stays() {}\n").unwrap();
    let moved = "#[\ntest\n]\nfn moves() {}\n#[test]\n#[ignore]\nfn ignored_moves() {}\n";
    fs::write(root.join("original/moved.rs"), moved).unwrap();
    let before = list(root);

    package(root, "successor", "mod moved;\n");
    fs::rename(
        root.join("original/moved.rs"),
        root.join("successor/moved.rs"),
    )
    .unwrap();
    fs::write(root.join("original/suite.rs"), "mod kept;\n").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = ['original', 'successor']\nresolver = '2'\n",
    )
    .unwrap();
    let renames = "[binaries]\n'original::suite' = ['original::suite', 'successor::suite']";
    assert_eq!(compare(&before, &list(root), renames).unwrap(), 3);
    assert_eq!(
        fs::read_to_string(root.join("successor/moved.rs")).unwrap(),
        moved
    );

    fs::remove_file(root.join("successor/moved.rs")).unwrap();
    fs::write(root.join("successor/suite.rs"), "").unwrap();
    let error = compare(&before, &list(root), renames).unwrap_err();
    assert_eq!(
        error.to_string(),
        "test census differs: base 3, candidate 1\nmissing (2):\noriginal::suite\tmoved::ignored_moves\noriginal::suite\tmoved::moves\nadded (0):\n\nchanged ignored flag (0):\n"
    );
}
