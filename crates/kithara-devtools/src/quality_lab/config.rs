use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::manifest::{Profile, Tool};

const CONFIG_REL: &str = ".config/quality-lab.toml";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QualityLabConfig {
    pub(super) schema_version: u32,
    pub(super) output_dir: String,
    pub(super) profiles: Profiles,
    pub(super) tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Profiles {
    pub(super) coverage: ProfileConfig,
    pub(super) scheduled: ProfileConfig,
    pub(super) manual: ProfileConfig,
}

impl Profiles {
    pub(super) const fn get(&self, profile: Profile) -> &ProfileConfig {
        match profile {
            Profile::Coverage => &self.coverage,
            Profile::Scheduled => &self.scheduled,
            Profile::Manual => &self.manual,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProfileConfig {
    pub(super) timeout_secs: u64,
    pub(super) tools: Vec<Tool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tools {
    #[serde(rename = "cargo-crap")]
    cargo_crap: ToolConfig,
    cha: ToolConfig,
    rustqual: ToolConfig,
    #[serde(rename = "cargo-dupes")]
    cargo_dupes: ToolConfig,
    pmat: ToolConfig,
}

impl Tools {
    pub(super) const fn get(&self, tool: Tool) -> &ToolConfig {
        match tool {
            Tool::CargoCrap => &self.cargo_crap,
            Tool::Cha => &self.cha,
            Tool::Rustqual => &self.rustqual,
            Tool::CargoDupes => &self.cargo_dupes,
            Tool::Pmat => &self.pmat,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolConfig {
    pub(super) timeout_secs: u64,
    pub(super) version: String,
}

impl QualityLabConfig {
    pub(super) fn load(workspace_root: &Path) -> Result<Self> {
        let path = workspace_root.join(CONFIG_REL);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read Quality Lab config: {}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("parse Quality Lab config: {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported Quality Lab config schema {}; expected {SCHEMA_VERSION}",
                self.schema_version
            );
        }
        if self.output_dir.trim().is_empty() {
            bail!("Quality Lab output_dir must not be empty");
        }
        let output = Path::new(&self.output_dir);
        if output.is_absolute()
            || output
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || output.components().next() != Some(Component::Normal("target".as_ref()))
        {
            bail!("Quality Lab output_dir must be a relative path below target/");
        }
        for profile in [Profile::Coverage, Profile::Scheduled, Profile::Manual] {
            let config = self.profiles.get(profile);
            if config.timeout_secs == 0 {
                bail!("Quality Lab profile `{profile}` timeout_secs must be positive");
            }
            let unique = config.tools.iter().copied().collect::<BTreeSet<_>>();
            if unique.len() != config.tools.len() {
                bail!("Quality Lab profile `{profile}` contains duplicate tools");
            }
        }
        for tool in Tool::ALL {
            let config = self.tools.get(tool);
            if config.timeout_secs == 0 {
                bail!("Quality Lab tool `{tool}` timeout_secs must be positive");
            }
            if config.version.trim().is_empty() {
                bail!("Quality Lab tool `{tool}` version must not be empty");
            }
        }
        if self.profiles.coverage.tools != [Tool::CargoCrap] {
            bail!("Quality Lab coverage profile must contain only cargo-crap");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    const VALID_CONFIG: &str = r#"
schema_version = 1
output_dir = "target/quality-lab"

[profiles.coverage]
timeout_secs = 1800
tools = ["cargo-crap"]

[profiles.scheduled]
timeout_secs = 900
tools = ["cha", "rustqual", "cargo-dupes"]

[profiles.manual]
timeout_secs = 1800
tools = ["cha", "rustqual", "cargo-dupes", "pmat"]

[tools.cargo-crap]
version = "0.3.1"
timeout_secs = 300

[tools.cha]
version = "1.20.0"
timeout_secs = 300

[tools.rustqual]
version = "1.8.1"
timeout_secs = 300

[tools.cargo-dupes]
version = "0.2.1"
timeout_secs = 300

[tools.pmat]
version = "3.24.2"
timeout_secs = 1800
"#;

    #[test]
    fn loads_valid_config() {
        let temp = tempdir().expect("tempdir");
        fs::create_dir(temp.path().join(".config")).expect("config dir");
        fs::write(temp.path().join(CONFIG_REL), VALID_CONFIG).expect("config");

        let config = QualityLabConfig::load(temp.path()).expect("valid config");

        assert_eq!(config.profiles.scheduled.tools.len(), 3);
        assert_eq!(config.tools.get(Tool::Rustqual).version, "1.8.1");
    }

    #[test]
    fn rejects_unknown_config_keys() {
        let temp = tempdir().expect("tempdir");
        fs::create_dir(temp.path().join(".config")).expect("config dir");
        fs::write(
            temp.path().join(CONFIG_REL),
            VALID_CONFIG.replace("schema_version = 1", "schema_version = 1\nmystery = true"),
        )
        .expect("config");

        let error = QualityLabConfig::load(temp.path()).expect_err("unknown key must fail");

        assert!(error.to_string().contains("parse Quality Lab config"));
    }

    #[test]
    fn coverage_profile_has_one_canonical_owner() {
        let temp = tempdir().expect("tempdir");
        fs::create_dir(temp.path().join(".config")).expect("config dir");
        fs::write(
            temp.path().join(CONFIG_REL),
            VALID_CONFIG.replace(
                "tools = [\"cargo-crap\"]",
                "tools = [\"cargo-crap\", \"cha\"]",
            ),
        )
        .expect("config");

        let error = QualityLabConfig::load(temp.path()).expect_err("mixed coverage must fail");

        assert!(
            error
                .to_string()
                .contains("coverage profile must contain only cargo-crap")
        );
    }
}
