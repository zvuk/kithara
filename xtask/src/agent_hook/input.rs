use std::io::{self, Read};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

const INPUT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub(super) struct HookInput {
    pub(super) hook_event_name: String,
    pub(super) tool_name: String,
    pub(super) tool_input: Value,
}

impl HookInput {
    pub(super) fn string_field(&self, name: &str) -> Result<&str> {
        self.tool_input
            .as_object()
            .with_context(|| format!("hook tool_input for {} must be an object", self.tool_name))?
            .get(name)
            .and_then(Value::as_str)
            .with_context(|| {
                format!(
                    "hook tool_input for {} requires string field `{name}`",
                    self.tool_name
                )
            })
    }

    pub(super) fn has_field(&self, name: &str) -> Result<bool> {
        Ok(self
            .tool_input
            .as_object()
            .with_context(|| format!("hook tool_input for {} must be an object", self.tool_name))?
            .get(name)
            .is_some_and(|value| !value.is_null()))
    }
}

pub(super) fn read() -> Result<HookInput> {
    let stdin = io::stdin();
    read_from(stdin.lock(), INPUT_BYTES)
}

fn read_from(reader: impl Read, limit: usize) -> Result<HookInput> {
    let mut body = String::new();
    reader
        .take(u64::try_from(limit)?.saturating_add(1))
        .read_to_string(&mut body)
        .context("read hook JSON from stdin")?;
    if body.len() > limit {
        bail!("hook JSON exceeds {limit} byte input limit");
    }
    parse(&body)
}

fn parse(body: &str) -> Result<HookInput> {
    serde_json::from_str(body).context("parse hook JSON from stdin")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use anyhow::Result;

    use super::{parse, read_from};

    #[test]
    fn parses_real_hook_discriminators() -> Result<()> {
        let input = parse(
            r#"{"hook_event_name":"PostToolUse","tool_name":"apply_patch","tool_input":{"command":"*** Begin Patch\n*** End Patch"}}"#,
        )?;

        assert_eq!(input.hook_event_name, "PostToolUse");
        assert_eq!(input.tool_name, "apply_patch");
        assert_eq!(
            input.string_field("command")?,
            "*** Begin Patch\n*** End Patch"
        );
        Ok(())
    }

    #[test]
    fn missing_hook_event_name_is_a_payload_error() {
        let error =
            parse(r#"{"tool_name":"Bash","tool_input":{}}"#).expect_err("missing event must fail");

        assert!(format!("{error:#}").contains("hook_event_name"));
    }

    #[test]
    fn rejects_input_at_the_reader_boundary() {
        let error = read_from(Cursor::new(br#"{\"too\":\"large\"}"#), 8)
            .expect_err("oversized input must fail before parsing");

        assert!(format!("{error:#}").contains("8 byte input limit"));
    }
}
