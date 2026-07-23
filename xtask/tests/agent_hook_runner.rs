#![cfg(unix)]

use std::{
    env, fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result};
use serde_json::Value;
use tempfile::TempDir;

const DEFAULT_CONFIG: &str = r#"
[ext.agent_hook]
destructive_git_override_env = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT"

[[ext.agent_hook.routes]]
event = "pre-tool-use"
tool_kind = "shell"
handler = "command-guard"

[[ext.agent_hook.routes]]
event = "post-tool-use"
tool_kind = "file-edit"
handler = "format-edited-paths"
"#;

struct Fixture {
    _temp: TempDir,
    cargo_log: PathBuf,
    format_log: PathBuf,
    path: std::ffi::OsString,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let bin = temp.path().join("bin");
        let cargo_log = temp.path().join("cargo.log");
        let format_log = temp.path().join("format.log");

        fs::create_dir_all(root.join(".config"))?;
        fs::create_dir_all(&bin)?;
        fs::write(root.join(".config/xtask.toml"), DEFAULT_CONFIG)?;
        fs::write(root.join("rustfmt.toml"), "edition = \"2024\"\n")?;
        write_executable(
            &bin.join("cargo"),
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$FAKE_CARGO_LOG"
exit 91
"#,
        )?;
        for formatter in ["rustup", "taplo", "tidy-json"] {
            write_executable(
                &bin.join(formatter),
                r#"#!/bin/sh
set -eu
printf '%s\t%s\n' "${0##*/}" "$*" >> "$FAKE_FORMAT_LOG"
"#,
            )?;
        }

        let mut paths = vec![bin];
        if let Some(system_path) = env::var_os("PATH") {
            paths.extend(env::split_paths(&system_path));
        }
        let path = env::join_paths(paths).context("construct fixture PATH")?;

        Ok(Self {
            _temp: temp,
            cargo_log,
            format_log,
            path,
            root,
        })
    }

    fn write_config(&self, config: &str) -> Result<()> {
        fs::write(self.root.join(".config/xtask.toml"), config)?;
        Ok(())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .arg("agent-hook")
            .current_dir(&self.root)
            .env("FAKE_CARGO_LOG", &self.cargo_log)
            .env("FAKE_FORMAT_LOG", &self.format_log)
            .env("KITHARA_AGENT_HOOK_ROOT", "/ignored/legacy/root")
            .env("KITHARA_AGENT_HOOK_CACHE", "/ignored/legacy/cache")
            .env("CLAUDE_PROJECT_DIR", "/ignored/claude/root")
            .env("PATH", &self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, input: &Value) -> Result<Output> {
        self.run_raw(&serde_json::to_string(input)?)
    }

    fn run_raw(&self, input: &str) -> Result<Output> {
        let mut child = self.command().spawn()?;
        child
            .stdin
            .take()
            .context("open hook stdin")?
            .write_all(input.as_bytes())?;
        child.wait_with_output().context("wait for agent hook")
    }

    fn assert_cargo_not_run(&self) {
        assert!(!self.cargo_log.exists(), "agent hook invoked Cargo");
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_executable(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[test]
fn configured_shell_route_denies_real_payload_without_legacy_envs() -> Result<()> {
    let fixture = Fixture::new()?;
    let output = fixture.run(&serde_json::json!({
        "session_id": "session-test",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": fixture.root,
        "tool_input": {"command": "cargo test"}
    }))?;

    assert_success(&output);
    let response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        response["hookSpecificOutput"]["hookEventName"],
        "PreToolUse"
    );
    assert_eq!(response["hookSpecificOutput"]["permissionDecision"], "deny");
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn routing_is_data_driven_and_unknown_discriminators_are_noops() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write_config(
        r#"
[ext.agent_hook]
destructive_git_override_env = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT"

[[ext.agent_hook.routes]]
event = "post-tool-use"
tool_kind = "file-edit"
handler = "format-edited-paths"
"#,
    )?;

    for input in [
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test"}
        }),
        serde_json::json!({
            "hook_event_name": "Stop",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test"}
        }),
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_input": {}
        }),
    ] {
        let output = fixture.run(&input)?;
        assert_success(&output);
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn malformed_payload_and_missing_config_are_visible_errors() -> Result<()> {
    let fixture = Fixture::new()?;
    let malformed = fixture.run_raw("not json")?;
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("parse hook JSON"));

    let missing_event = fixture.run(&serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {"command": "cargo test"}
    }))?;
    assert!(!missing_event.status.success());
    assert!(String::from_utf8_lossy(&missing_event.stderr).contains("hook_event_name"));

    fs::remove_file(fixture.root.join(".config/xtask.toml"))?;
    let missing_config = fixture.run(&serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "cargo test"}
    }))?;
    assert!(!missing_config.status.success());
    assert!(String::from_utf8_lossy(&missing_config.stderr).contains("ext.agent_hook"));
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn matched_tools_require_their_payload_fields() -> Result<()> {
    let fixture = Fixture::new()?;
    for input in [
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {}
        }),
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "apply_patch",
            "tool_input": {}
        }),
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {}
        }),
    ] {
        let output = fixture.run(&input)?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires string field"));
    }
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn codex_apply_patch_formats_added_updated_and_moved_paths_once() -> Result<()> {
    let fixture = Fixture::new()?;
    fs::create_dir_all(fixture.root.join("src"))?;
    fs::create_dir_all(fixture.root.join(".config"))?;
    fs::write(fixture.root.join("src/added.rs"), "fn added() {}\n")?;
    fs::write(fixture.root.join(".config/edited.toml"), "value = true\n")?;
    fs::write(fixture.root.join("moved.json"), "{}\n")?;
    fs::write(fixture.root.join("Cargo.toml"), "[workspace]\n")?;

    let patch = "*** Begin Patch\n\
                 *** Add File: src/added.rs\n\
                 +fn added() {}\n\
                 *** Update File: .config/edited.toml\n\
                 @@\n\
                 -value = false\n\
                 +value = true\n\
                 *** Update File: old.json\n\
                 *** Move to: moved.json\n\
                 @@\n\
                 -{\"old\":true}\n\
                 +{}\n\
                 *** Delete File: deleted.rs\n\
                 *** Update File: .config/edited.toml\n\
                 @@\n\
                 -value = true\n\
                 +value = false\n\
                 *** Update File: Cargo.toml\n\
                 @@\n\
                 -[package]\n\
                 +[workspace]\n\
                 *** End Patch";
    let output = fixture.run(&serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "apply_patch",
        "tool_input": {"command": patch}
    }))?;

    assert_success(&output);
    let log = fs::read_to_string(&fixture.format_log)?;
    let lines = log.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "unexpected formatter calls: {log}");
    assert!(lines[0].starts_with("rustup\trun nightly rustfmt --edition 2024"));
    assert!(lines[0].ends_with("src/added.rs"));
    assert!(lines[1].starts_with("taplo\tformat "));
    assert!(lines[1].ends_with(".config/edited.toml"));
    assert!(lines[2].starts_with("tidy-json\t--indent 2 --write "));
    assert!(lines[2].ends_with("moved.json"));
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn direct_file_edit_is_path_scoped_and_cargo_free() -> Result<()> {
    let fixture = Fixture::new()?;
    let path = fixture.root.join("edited.jsonc");
    fs::write(&path, "{}\n")?;
    let output = fixture.run(&serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": {"file_path": path}
    }))?;

    assert_success(&output);
    let log = fs::read_to_string(&fixture.format_log)?;
    assert!(log.starts_with("tidy-json\t--indent 2 --write "));
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn apply_patch_path_escape_is_a_policy_error() -> Result<()> {
    let fixture = Fixture::new()?;
    let output = fixture.run(&serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "apply_patch",
        "tool_input": {
            "command": "*** Begin Patch\n*** Update File: ../outside.rs\n@@\n-old\n+new\n*** End Patch"
        }
    }))?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("repository root"));
    assert!(!fixture.format_log.exists());
    fixture.assert_cargo_not_run();
    Ok(())
}

#[test]
fn old_agent_hook_subcommands_are_rejected() -> Result<()> {
    for subcommand in ["pre-bash", "post-edit", "install"] {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["agent-hook", subcommand])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;

        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    }
    Ok(())
}
