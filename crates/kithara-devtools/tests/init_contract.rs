use std::{fs, path::Path};

use kithara_devtools::{
    CoreCommand, Ctx,
    common::{baseline::Baseline, project::ProjectConfig},
    init::InitArgs,
};
use tempfile::tempdir;

fn ctx(root: &Path, name: &str) -> Ctx {
    let mut config = ProjectConfig::default();
    config.project.name = name.to_string();
    Ctx {
        config,
        root: root.to_path_buf(),
    }
}

fn run_init(root: &Path, force: bool) -> anyhow::Result<()> {
    let command = CoreCommand::Init(InitArgs { force });
    kithara_devtools::run(&command, &ctx(root, "demo-project"))
}

#[test]
fn init_scaffolds_config_and_empty_baselines() {
    let temp = tempdir().expect("tempdir");

    run_init(temp.path(), false).expect("run init");

    let config_path = temp.path().join(".config/xtask.toml");
    assert!(config_path.is_file());

    let config = ProjectConfig::load(temp.path()).expect("generated config parses");
    assert_eq!(config.project.name, "demo-project");
    assert!(!config.project.name.is_empty());

    for dir in [".config/arch", ".config/style", ".config/idioms"] {
        let config_dir = temp.path().join(dir);
        let baseline_path = config_dir.join("baseline.toml");
        assert!(baseline_path.is_file());
        let baseline = Baseline::load(&config_dir).expect("empty baseline parses");
        assert!(baseline.checks.is_empty());
    }
}

#[test]
fn init_requires_force_to_overwrite_existing_targets() {
    let temp = tempdir().expect("tempdir");
    run_init(temp.path(), false).expect("first init");

    let config_path = temp.path().join(".config/xtask.toml");
    fs::write(&config_path, "not toml").expect("replace config");

    let error = run_init(temp.path(), false).expect_err("second init requires force");
    let message = format!("{error:#}");
    assert!(message.contains(".config/xtask.toml"));
    assert!(message.contains(".config/arch/baseline.toml"));

    run_init(temp.path(), true).expect("force overwrites");

    let config = ProjectConfig::load(temp.path()).expect("forced config parses");
    assert_eq!(config.project.name, "demo-project");
}
