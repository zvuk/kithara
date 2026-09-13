use std::path::Path;

use anyhow::Result;

use super::{host::CiHost, pins::CiPins};

/// One CI machine described by its own profile plus the reviewed build pins.
#[derive(Clone, Debug)]
pub(crate) struct CiConfig {
    pub(crate) host: CiHost,
    pub(crate) pins: CiPins,
}

impl CiConfig {
    pub(crate) fn load(host: &Path, pins: &Path) -> Result<Self> {
        Ok(Self {
            host: CiHost::load(host)?,
            pins: CiPins::load(pins)?,
        })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.host.validate()?;
        self.pins.validate()
    }

    pub(crate) fn validate_macos_layout(&self) -> Result<()> {
        self.host.validate_macos_layout()?;
        self.pins.validate()
    }
}

#[cfg(test)]
pub(crate) fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the workspace")
}

#[cfg(test)]
pub(crate) fn fixture() -> CiConfig {
    CiConfig::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ci-mac-host.toml"),
        &workspace_root().join(".config/ci-pins.toml"),
    )
    .expect("fixture host profile and tracked pins must load")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Keys nextest reads on a profile. Inside a `junit` table it drops them
    /// with a warning, which is how `[profile.ci.junit]` swallowed two of them.
    const PROFILE_KEYS: &[&str] = &["fail-fast", "leak-timeout", "slow-timeout", "test-threads"];

    #[test]
    fn the_ci_junit_report_lands_where_the_lanes_collect_it() {
        let nextest: toml::Value = toml::from_str(
            &fs::read_to_string(workspace_root().join(".config/nextest.toml")).unwrap(),
        )
        .unwrap();
        let junit = nextest["profile"]["ci"]["junit"].as_table().unwrap();

        let path = junit["path"].as_str().unwrap();
        assert!(
            !path.contains('/'),
            "nextest resolves junit.path against the profile store directory, so `{path}` \
             nests the report under `target/nextest/ci/` twice and CI collects nothing"
        );
        for key in PROFILE_KEYS {
            assert!(
                !junit.contains_key(*key),
                "`{key}` belongs on `[profile.ci]`; inside the junit table nextest ignores it"
            );
        }

        let produced = format!("target/nextest/ci/{path}");
        for lane in ["apple", "linux", "windows"] {
            let yaml = fs::read_to_string(workspace_root().join(format!(".gitlab/ci/{lane}.yml")))
                .unwrap();
            let declared = yaml
                .lines()
                .filter_map(|line| line.trim().strip_prefix("junit:"))
                .map(str::trim)
                .filter(|value| value.starts_with("target/nextest/"));
            for value in declared {
                assert_eq!(
                    value, produced,
                    "{lane}.yml collects a report that nextest does not write"
                );
            }
        }
    }

    /// The retry is what keeps the failing attempt: nextest records it inside
    /// `flakyFailure`, and the lane's verdict fails on a retried pass anyway.
    /// Without the retry the same throw leaves one red attempt and no record
    /// that the test reached a pass on the next one.
    #[test]
    fn the_judged_profile_keeps_the_attempt_it_retried() {
        let nextest: toml::Value = toml::from_str(
            &fs::read_to_string(workspace_root().join(".config/nextest.toml")).unwrap(),
        )
        .unwrap();

        let retries = nextest["profile"]["ci"].get("retries");
        assert!(
            retries.is_some_and(|value| value.as_integer().is_some_and(|count| count > 0)),
            "the CI profile must retry, or a flake leaves no record of being one"
        );
    }

    #[test]
    fn tracked_pins_are_valid_on_every_build_platform() {
        CiPins::load(&workspace_root().join(".config/ci-pins.toml")).unwrap();
    }

    #[test]
    fn fixture_profile_matches_the_host_contract() {
        fixture().validate_macos_layout().unwrap();
    }

    #[test]
    fn installed_copies_round_trip_through_toml() {
        let config = fixture();
        let directory = tempfile::tempdir().unwrap();
        let host = directory.path().join("mac-host.toml");
        let pins = directory.path().join("pins.toml");
        config.host.write(&host).unwrap();
        config.pins.write(&pins).unwrap();

        let installed = CiConfig::load(&host, &pins).unwrap();
        assert_eq!(installed.host.host_root, config.host.host_root);
        assert_eq!(installed.pins.cargo_tools, config.pins.cargo_tools);
        assert_eq!(installed.pins.brew_formulae, config.pins.brew_formulae);
    }
}
