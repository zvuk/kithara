use std::io::{self, Read};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(crate) struct HookInput {
    #[serde(default)]
    pub(crate) tool_name: String,
    #[serde(default)]
    pub(crate) cwd: String,
    #[serde(default)]
    pub(crate) tool_input: HookToolInput,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct HookToolInput {
    #[serde(default)]
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) timeout: Option<Value>,
    #[serde(default)]
    pub(crate) file_path: Option<String>,
}

pub(crate) fn read() -> Result<HookInput> {
    let mut body = String::new();
    io::stdin()
        .read_to_string(&mut body)
        .context("read hook JSON from stdin")?;
    if body.trim().is_empty() {
        return Ok(HookInput {
            tool_name: String::new(),
            cwd: String::new(),
            tool_input: HookToolInput::default(),
        });
    }
    serde_json::from_str(&body).context("parse hook JSON from stdin")
}
