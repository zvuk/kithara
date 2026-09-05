use std::collections::BTreeMap;

use serde::Deserialize;

/// External programs this project addresses by role rather than by a literal
/// spelled at the call site.
///
/// A role absent from the table resolves to its own name, so an empty table
/// reproduces a hard-coded literal exactly: nothing has to be configured for a
/// fresh project to work, and no entry exists that can only equal its default.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct ToolsConfig {
    roles: BTreeMap<String, ToolEntry>,
}

/// What one role is, where to get it, and what pins its version.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolEntry {
    /// Program to spawn. Empty means the role name itself.
    pub program: String,
    /// What to tell an operator who does not have it.
    pub install_hint: String,
    /// Key in `.config/ci-pins.toml` `[cargo_tools]` pinning this role's
    /// version. Empty for a platform toolchain the machine owns rather than
    /// this repository.
    pub pin: String,
}

impl ToolsConfig {
    /// The program for `role`, falling back to the role name.
    #[must_use]
    pub fn program<'a>(&'a self, role: &'a str) -> &'a str {
        self.roles
            .get(role)
            .map(|entry| entry.program.as_str())
            .filter(|program| !program.is_empty())
            .unwrap_or(role)
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
