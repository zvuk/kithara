use std::{fs, path::Path};

use kithara_devtools::common::{
    project::ProjectConfig,
    scope::Scope,
    walker::{relative_to, workspace_rs_files_scoped},
};
use tempfile::tempdir;

fn write_config(root: &Path, text: &str) {
    let config_dir = root.join(".config");
    fs::create_dir_all(&config_dir).expect("create config dir");
    fs::write(config_dir.join("xtask.toml"), text).expect("write config");
}

#[test]
fn missing_config_file_yields_defaults() {
    let temp = tempdir().expect("tempdir");

    let config = ProjectConfig::load(temp.path()).expect("load missing config");

    assert!(config.workspace_scan.exclude.is_empty());
}

#[test]
fn unknown_section_is_a_typed_error() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[projct]
name = "typo"
"#,
    );

    let error = ProjectConfig::load(temp.path()).expect_err("unknown section fails");
    let message = format!("{error:#}");

    assert!(
        message.contains("projct"),
        "error did not mention offending token: {message}"
    );
}

#[test]
fn top_level_consumer_section_is_rejected() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[android]
ffi_crate = "kithara-ffi"
"#,
    );

    let error = ProjectConfig::load(temp.path()).expect_err("top-level consumer section fails");
    let message = format!("{error:#}");

    assert!(
        message.contains("android"),
        "error did not mention offending token: {message}"
    );
}

#[test]
fn ext_table_accepts_consumer_sections() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[ext.android]
ffi_crate = "kithara-ffi"

[ext.local_tool]
enabled = true
"#,
    );

    let config = ProjectConfig::load(temp.path()).expect("load ext passthrough config");

    assert!(config.ext.contains_key("android"));
    assert!(config.ext.contains_key("local_tool"));
}

#[test]
fn workspace_scan_parses() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[workspace-scan]
exclude = ["foo/**"]
"#,
    );

    let config = ProjectConfig::load(temp.path()).expect("load workspace scan config");

    assert_eq!(config.workspace_scan.exclude, ["foo/**"]);
}

#[test]
fn workspace_scan_excludes_walked_files() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[workspace-scan]
exclude = ["crates/generated/**"]
"#,
    );
    let kept_dir = temp.path().join("crates/app/src");
    let excluded_dir = temp.path().join("crates/generated/src");
    fs::create_dir_all(&kept_dir).expect("create kept dir");
    fs::create_dir_all(&excluded_dir).expect("create excluded dir");
    fs::write(kept_dir.join("lib.rs"), "").expect("write kept file");
    fs::write(excluded_dir.join("lib.rs"), "").expect("write excluded file");

    let files =
        workspace_rs_files_scoped(temp.path(), &Scope::default()).expect("walk scoped files");
    let rels: Vec<String> = files
        .iter()
        .map(|path| {
            relative_to(temp.path(), path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();

    assert_eq!(rels, ["crates/app/src/lib.rs"]);
}
