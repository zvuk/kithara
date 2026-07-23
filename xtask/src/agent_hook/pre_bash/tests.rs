use super::{deny_reason_for_bash, shell::shell_tokens};

const OVERRIDE_ENV: &str = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT";

fn denied(command: &str) -> bool {
    deny_reason_for_bash(command, false, OVERRIDE_ENV).is_some()
}

#[test]
fn denies_broad_raw_test_commands() {
    assert!(denied("cargo test"));
    assert!(denied("cargo --locked test"));
    assert!(denied("cargo -q test"));
    assert!(denied("cargo --color never test"));
    assert!(denied("cargo --config net.git-fetch-with-cli=true test"));
    assert!(denied("cargo test --workspace --"));
    assert!(denied("cargo test --workspace --profile test-release"));
    assert!(denied(
        "cargo nextest run --workspace --cargo-profile test-release"
    ));
    assert!(denied("cargo nextest run --workspace --run-ignored all"));
    assert!(denied("cargo nextest run --workspace --no-tests pass"));
    assert!(denied(
        "cargo nextest run --workspace --failure-output final"
    ));
}

#[test]
fn denies_broad_raw_tests_through_rtk() {
    assert!(denied("rtk cargo test"));
    assert!(denied("rtk proxy cargo test --workspace"));
    assert!(denied(
        "rtk env CARGO_TARGET_DIR=/tmp cargo nextest run --workspace"
    ));
    assert!(denied(
        "FOO=1 rtk env -- BAR=2 cargo nextest run --workspace"
    ));
}

#[test]
fn denies_rtk_options_and_command_wrappers() {
    assert!(denied("rtk --skip-env cargo test"));
    assert!(denied("rtk --verbose cargo test"));
    assert!(denied("rtk cargo --skip-env test"));
    assert!(denied("rtk proxy --skip-env cargo test"));
    assert!(denied("rtk proxy 'cargo test'"));
    assert!(denied("rtk test cargo test"));
    assert!(denied("rtk test 'cargo test'"));
    assert!(denied("rtk err cargo test"));
    assert!(denied("rtk err 'cargo test'"));
    assert!(denied("rtk summary git reset --hard HEAD"));
    assert!(denied("rtk summary 'git reset --hard HEAD'"));
    assert!(denied("rtk run cargo test"));
    assert!(denied("rtk run -c 'cargo test'"));
    assert!(denied("rtk --skip-env git reset --hard HEAD"));
    assert!(denied("rtk git -C . reset --hard HEAD"));
    assert!(denied("rtk git -C. reset --hard HEAD"));
    assert!(denied("rtk git -cfoo.bar=baz reset --hard HEAD"));
    assert!(denied("rtk git -C . --skip-env reset --hard HEAD"));
    assert!(denied("rtk env -u FOO cargo test"));
    assert!(denied("rtk env -i cargo test"));
    assert!(denied("rtk env -C . cargo test"));
    assert!(denied("rtk env -P /bin cargo test"));
    assert!(denied("rtk env -S 'cargo test'"));
}

#[test]
fn denies_destructive_git_through_rtk() {
    assert!(denied("rtk git reset --hard HEAD"));
    assert!(denied("rtk proxy git clean -fdx"));
    assert!(denied(
        "rtk env GIT_OPTIONAL_LOCKS=0 git checkout -- Cargo.toml"
    ));
}

#[test]
fn denies_timed_full_harness_through_rtk() {
    assert!(denied("timeout 60s rtk cargo xtask test"));
    assert!(deny_reason_for_bash("rtk just test", true, OVERRIDE_ENV).is_some());
}

#[test]
fn allows_scoped_raw_test_probes_through_rtk() {
    assert!(!denied("rtk cargo test -p xtask agent_hook"));
    assert!(!denied("rtk cargo --skip-env test -p xtask agent_hook"));
    assert!(!denied(
        "rtk proxy cargo nextest run -p kithara-platform -E 'test(foo)'"
    ));
    assert!(!denied(
        "rtk env CARGO_TARGET_DIR=/tmp cargo nextest run --workspace -E 'test(foo)'"
    ));
}

#[test]
fn allows_scoped_raw_test_probes() {
    assert!(!denied("cargo test -p xtask agent_hook"));
    assert!(!denied("cargo test -pxtask agent_hook"));
    assert!(!denied("cargo test -mCargo.toml agent_hook"));
    assert!(!denied(
        "cargo nextest run -p kithara-platform -E 'test(foo)'"
    ));
    assert!(!denied("cargo nextest run -Etest(foo)"));
    assert!(!denied("cargo nextest run --filterset 'test(foo)'"));
    assert!(!denied("cargo nextest run test(foo)"));
    assert!(!denied("cargo nextest run --workspace -E 'test(foo)'"));
}

#[test]
fn denies_formatter_bypass_commands() {
    assert!(denied("rustfmt crates/foo/src/lib.rs"));
    assert!(denied("cargo sort --check --workspace"));
    assert!(denied("taplo format --check Cargo.toml"));
    assert!(denied("mdfmt --check AGENTS.md"));
}

#[test]
fn allows_formatter_version_probes() {
    assert!(!denied("taplo --version"));
    assert!(!denied("mdfmt --help"));
}

#[test]
fn denies_timeout_around_full_harness() {
    assert!(denied("timeout 120s just test"));
    assert!(denied(
        "timeout --kill-after=5s 120s cargo xtask test --lane workspace"
    ));
    assert!(deny_reason_for_bash("just test", true, OVERRIDE_ENV).is_some());
}

#[test]
fn allows_timeout_around_scoped_probe() {
    assert!(!denied(
        "timeout 30s cargo nextest run -p xtask -E 'test(agent_hook)'"
    ));
}

#[test]
fn denies_destructive_git_without_override() {
    assert!(denied("git reset --hard HEAD"));
    assert!(denied("git clean -fdx"));
    assert!(denied("git checkout -- crates/foo/src/lib.rs"));
}

#[test]
fn allows_only_exact_segment_scoped_destructive_git_override() {
    assert!(!denied(
        "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT=1 git reset --hard HEAD"
    ));
    assert!(!denied(
        "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT=1 rtk run -c 'git reset --hard HEAD'"
    ));
    assert!(denied(
        "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT=10 git reset --hard HEAD"
    ));
    assert!(denied(
        "echo KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT=1; git reset --hard HEAD"
    ));
    assert!(denied("git clean -fdx # kithara-allow-destructive-git"));
}

#[test]
fn destructive_git_override_name_comes_from_config() {
    let custom = "PROJECT_APPROVED_DESTRUCTIVE_GIT";

    assert!(
        deny_reason_for_bash(
            "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT=1 git reset --hard HEAD",
            false,
            custom,
        )
        .is_some()
    );
    assert!(
        deny_reason_for_bash(
            "PROJECT_APPROVED_DESTRUCTIVE_GIT=1 git reset --hard HEAD",
            false,
            custom,
        )
        .is_none()
    );
}

#[test]
fn scans_later_shell_segments() {
    assert!(denied(
        "cargo xtask format --check && cargo nextest run --workspace"
    ));
    assert!(denied("echo ok\ncargo test"));
    assert!(denied("echo ok & cargo test"));
}

#[test]
fn tokenizes_quotes_and_separators() {
    assert_eq!(
        shell_tokens("FOO=1 cargo nextest run -E 'test(foo)' && just test"),
        [
            "FOO=1",
            "cargo",
            "nextest",
            "run",
            "-E",
            "test(foo)",
            "&&",
            "just",
            "test"
        ]
    );
}
