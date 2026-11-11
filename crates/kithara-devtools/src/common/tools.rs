use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

/// External programs this project addresses by role rather than by a literal
/// spelled at the call site.
///
/// A role absent from the table resolves to its own name, so an empty table
/// reproduces a hard-coded literal exactly: nothing has to be configured for a
/// fresh project to work, and no entry exists that can only equal its default.
#[derive(Debug, Default, Deserialize, Clone, kithara_config::Config)]
#[serde(transparent)]
#[config(builder(none))]
pub struct ToolsConfig {
    #[config(value)]
    roles: BTreeMap<String, ToolEntry>,
}

/// What one role is, where to get it, and what pins its version.
#[derive(Debug, Default, Deserialize, Clone, kithara_config::Config)]
#[serde(default, deny_unknown_fields)]
#[config(builder(none), fields(value))]
pub struct ToolEntry {
    /// What to tell an operator who does not have it.
    pub install_hint: String,
    /// Key in `.config/ci-pins.toml` `[cargo_tools]` pinning this role's
    /// version. Empty for a platform toolchain the machine owns rather than
    /// this repository.
    pub pin: String,
    /// Program to spawn. Empty means the role name itself.
    pub program: String,
}

impl ToolsConfig {
    /// Load only external program settings from the project configuration.
    ///
    /// # Errors
    /// Returns an error when the file or its tools table cannot be read or parsed.
    pub fn load(workspace_root: &Path) -> Result<Self> {
        let mut table: toml::Table = super::project::load_optional_config(
            &workspace_root.join(crate::consts::PROJECT_CONFIG_REL),
            "project tools config",
        )?;
        table.remove("tools").map_or_else(
            || Ok(Self::default()),
            |tools| tools.try_into().context("parse project tools config"),
        )
    }

    /// What to print when `role` is missing, falling back to the caller's
    /// compiled hint when the config carries none.
    #[must_use]
    pub fn install_hint<'a>(&'a self, role: &str, fallback: &'a str) -> &'a str {
        self.roles
            .get(role)
            .map(|entry| entry.install_hint.as_str())
            .filter(|hint| !hint.is_empty())
            .unwrap_or(fallback)
    }

    /// Every role that claims a version pin, as `(role, pin key)`.
    pub fn pinned_roles(&self) -> impl Iterator<Item = (&str, &str)> {
        self.roles.iter().filter_map(|(role, entry)| {
            (!entry.pin.is_empty()).then_some((role.as_str(), entry.pin.as_str()))
        })
    }

    /// The program for `role`, falling back to the role name.
    #[must_use]
    pub fn program<'a>(&'a self, role: &'a str) -> &'a str {
        self.roles
            .get(role)
            .map(|entry| entry.program.as_str())
            .filter(|program| !program.is_empty())
            .unwrap_or(role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(contents: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("project directory");
        std::fs::create_dir(root.path().join(".config")).expect("config directory");
        std::fs::write(
            root.path().join(crate::consts::PROJECT_CONFIG_REL),
            contents,
        )
        .expect("project config");
        root
    }

    #[test]
    fn loading_tools_does_not_parse_unrelated_project_settings() {
        let root = project(
            "[tools.kache]\nprogram = '/pinned tools/kache'\n[stress.modes.future]\nfuture_budget = 1\n",
        );

        assert!(super::super::project::ProjectConfig::load(root.path()).is_err());
        let tools = ToolsConfig::load(root.path()).expect("tools settings are independent");
        assert_eq!(tools.program("kache"), "/pinned tools/kache");
    }

    #[test]
    fn loading_tools_preserves_missing_configuration_defaults() {
        let absent = tempfile::tempdir().expect("project without config");
        assert_eq!(
            ToolsConfig::load(absent.path()).unwrap().program("kache"),
            "kache"
        );
        for contents in ["", "[stress.modes.future]\nfuture_budget = 1\n"] {
            let root = project(contents);
            assert_eq!(
                ToolsConfig::load(root.path()).unwrap().program("kache"),
                "kache"
            );
        }
    }

    #[test]
    fn loading_tools_rejects_invalid_owned_settings_and_toml() {
        for contents in [
            "tools = 7",
            "[tools.kache]\nprogram = 7",
            "[tools.kache]\nfuture_program = 'kache'",
            "[stress",
        ] {
            let root = project(contents);
            assert!(ToolsConfig::load(root.path()).is_err(), "{contents}");
        }
    }

    #[test]
    fn an_unlisted_role_resolves_to_its_own_name() {
        let tools = ToolsConfig::default();

        assert_eq!(tools.program("ast-grep"), "ast-grep");
    }

    #[test]
    fn an_unconfigured_role_keeps_the_compiled_install_hint() {
        let tools = ToolsConfig::default();

        assert_eq!(
            tools.install_hint("typos", "cargo install typos-cli"),
            "cargo install typos-cli"
        );
    }

    #[test]
    fn a_configured_install_hint_wins_over_the_compiled_one() {
        let tools: ToolsConfig = toml::from_str(
            r#"
            [typos]
            install_hint = "brew install typos-cli"
            "#,
        )
        .expect("the tools table parses");

        assert_eq!(
            tools.install_hint("typos", "cargo install typos-cli"),
            "brew install typos-cli"
        );
    }

    #[test]
    fn a_listed_role_resolves_to_its_configured_program() {
        let tools: ToolsConfig = toml::from_str(
            r#"
            [ast-grep]
            program = "/opt/pinned/bin/ast-grep"
            install_hint = "cargo install ast-grep --locked"
            pin = "ast-grep"
            "#,
        )
        .expect("the tools table parses");

        assert_eq!(tools.program("ast-grep"), "/opt/pinned/bin/ast-grep");
        assert_eq!(
            tools.pinned_roles().collect::<Vec<_>>(),
            vec![("ast-grep", "ast-grep")]
        );
    }

    /// An entry that names no program is a hint carrier, not a rename. Falling
    /// back to the role name keeps it behaving exactly like a bare literal.
    #[test]
    fn an_entry_without_a_program_still_resolves_to_the_role_name() {
        let tools: ToolsConfig = toml::from_str(
            r#"
            [typos]
            install_hint = "cargo install typos-cli"
            "#,
        )
        .expect("the tools table parses");

        assert_eq!(tools.program("typos"), "typos");
        assert_eq!(tools.pinned_roles().count(), 0);
    }

    #[test]
    fn an_unknown_key_inside_a_role_is_refused() {
        let error = toml::from_str::<ToolsConfig>(
            r#"
            [ast-grep]
            programme = "ast-grep"
            "#,
        )
        .expect_err("an unknown key must be refused, not ignored");

        assert!(error.to_string().contains("programme"));
    }
}
