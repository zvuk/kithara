use std::{fmt, fs, path::Path};

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Profile {
    Coverage,
    Scheduled,
    Manual,
}

impl fmt::Display for Profile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Coverage => "coverage",
            Self::Scheduled => "scheduled",
            Self::Manual => "manual",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Tool {
    CargoCrap,
    Cha,
    Rustqual,
    CargoDupes,
    Pmat,
}

impl Tool {
    pub(super) const ALL: [Self; 5] = [
        Self::CargoCrap,
        Self::Cha,
        Self::Rustqual,
        Self::CargoDupes,
        Self::Pmat,
    ];

    pub(super) const fn program(self) -> &'static str {
        match self {
            Self::CargoCrap => "cargo-crap",
            Self::Cha => "cha",
            Self::Rustqual => "rustqual",
            Self::CargoDupes => "cargo-dupes",
            Self::Pmat => "pmat",
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CargoCrap => "cargo-crap",
            Self::Cha => "cha",
            Self::Rustqual => "rustqual",
            Self::CargoDupes => "cargo-dupes",
            Self::Pmat => "pmat",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Status {
    Clean,
    Findings,
    Skipped,
    ToolError,
}

impl fmt::Display for Status {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Clean => "clean",
            Self::Findings => "findings",
            Self::Skipped => "skipped",
            Self::ToolError => "tool_error",
        })
    }
}

#[derive(Debug, Serialize)]
pub(super) struct InvocationManifest {
    pub(super) artifact: String,
    pub(super) command: Vec<String>,
    pub(super) duration_ms: u64,
    pub(super) exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
pub(super) struct ToolManifest {
    pub(super) schema_version: u32,
    pub(super) revision: String,
    pub(super) profile: Profile,
    pub(super) tool: Tool,
    pub(super) tool_version: String,
    pub(super) status: Status,
    pub(super) duration_ms: u64,
    pub(super) invocations: Vec<InvocationManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) note: Option<String>,
}

impl ToolManifest {
    pub(super) fn write(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self).context("serialize Quality Lab manifest")?;
        fs::write(path, format!("{json}\n"))
            .with_context(|| format!("write Quality Lab manifest: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn manifest_uses_stable_kebab_and_snake_case_names() {
        let manifest = ToolManifest {
            schema_version: 1,
            revision: "abc123".to_owned(),
            profile: Profile::Scheduled,
            tool: Tool::CargoDupes,
            tool_version: "0.2.1".to_owned(),
            status: Status::ToolError,
            duration_ms: 12,
            invocations: vec![InvocationManifest {
                artifact: "report.json".to_owned(),
                command: vec!["cargo-dupes".to_owned(), "report".to_owned()],
                duration_ms: 10,
                exit_code: Some(2),
            }],
            note: None,
        };

        let value = serde_json::to_value(manifest).expect("manifest JSON");

        assert_eq!(value["profile"], Value::String("scheduled".to_owned()));
        assert_eq!(value["tool"], Value::String("cargo-dupes".to_owned()));
        assert_eq!(value["status"], Value::String("tool_error".to_owned()));
        assert!(value.get("note").is_none());
    }
}
