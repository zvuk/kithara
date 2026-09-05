use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use kithara_devtools::common::tools::ToolsConfig;
use serde::{Deserialize, Serialize};

/// Repository-relative location of the reviewed build pins.
pub(crate) const PINS_PATH: &str = ".config/ci-pins.toml";

/// Reviewed build contract: everything a CI job installs, pulls, or pins.
/// Machine-specific paths and accounts live in [`super::CiHost`].
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiPins {
    pub(crate) actions_runner_linux_amd64_sha256: String,
    pub(crate) actions_runner_linux_arm64_sha256: String,
    pub(crate) actions_runner_version: String,
    pub(crate) actions_runner_windows_sha256: String,
    pub(crate) android_avd: String,
    pub(crate) android_build_tools_version: String,
    pub(crate) android_commandline_tools_linux_sha256: String,
    pub(crate) android_commandline_tools_sha256: String,
    pub(crate) android_commandline_tools_version: String,
    pub(crate) android_ndk_version: String,
    pub(crate) android_platform_version: u32,
    pub(crate) brew_casks: Vec<String>,
    pub(crate) brew_formulae: Vec<String>,
    /// Chromium the browser lane may run, and the version its `chromedriver` must
    /// report. The image installs both from Debian as one version-matched pair,
    /// so this pin is a review gate on what a rebuild brought in rather than a
    /// download coordinate: a mismatch fails the lane by name instead of letting
    /// a browser change alter test results unnoticed.
    pub(crate) chromium_version: String,
    pub(crate) cmake_linux_amd64_sha256: String,
    pub(crate) cmake_linux_arm64_sha256: String,
    pub(crate) cmake_version: String,
    pub(crate) cmake_windows_amd64_sha256: String,
    pub(crate) expected_xcode_version: String,
    pub(crate) geckodriver_linux_amd64_sha256: String,
    pub(crate) geckodriver_linux_arm64_sha256: String,
    pub(crate) geckodriver_version: String,
    pub(crate) git_windows_sha256: String,
    /// Named in full rather than built from a version: the release tag and the
    /// file name spell the same version differently.
    pub(crate) git_windows_url: String,
    pub(crate) gitlab_runner_version: String,
    pub(crate) gitleaks_linux_amd64_sha256: String,
    pub(crate) gitleaks_linux_arm64_sha256: String,
    pub(crate) gitleaks_version: String,
    pub(crate) linux_base_digest: String,
    pub(crate) linux_android_image: String,
    pub(crate) linux_android_runner_image: String,
    pub(crate) linux_image: String,
    pub(crate) linux_runner_image: String,
    /// lockbud is a rustc driver rather than a crates.io package, so it is
    /// pinned by commit and by the nightly it links `rustc_driver` against.
    /// That nightly also has to compile the workspace it reads.
    pub(crate) lockbud_rev: String,
    pub(crate) lockbud_toolchain: String,
    /// Build the guest macOS must report. The CI VM is built locally from the
    /// matching Apple restore image instead of pulled from a registry.
    pub(crate) macos_guest_build: String,
    pub(crate) msrv_toolchain: String,
    pub(crate) nightly_toolchain: String,
    pub(crate) rtsan_linux_amd64_sha256: String,
    pub(crate) rtsan_linux_arm64_sha256: String,
    /// Release tag of the prebuilt realtime-sanitizer runtimes the stable lane
    /// links. It tracks the LLVM the libraries were cut from, not a toolchain
    /// pinned here.
    pub(crate) rtsan_version: String,
    pub(crate) rustup_version: String,
    pub(crate) rustup_windows_sha256: String,
    pub(crate) stable_toolchain: String,
    pub(crate) windows_eval_iso_sha256: String,
    pub(crate) windows_eval_iso_url: String,
    /// Serialised last: TOML requires tables after plain values.
    pub(crate) cargo_tools: BTreeMap<String, String>,
}

impl CiPins {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading CI pins {}", path.display()))?;
        let pins: Self =
            toml::from_str(&text).with_context(|| format!("parsing CI pins {}", path.display()))?;
        pins.validate()?;
        Ok(pins)
    }

    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing CI pins")?;
        fs::write(path, text).with_context(|| format!("writing CI pins {}", path.display()))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        for (name, value) in [
            (
                "actions_runner_version",
                self.actions_runner_version.as_str(),
            ),
            ("android_avd", self.android_avd.as_str()),
            (
                "android_build_tools_version",
                self.android_build_tools_version.as_str(),
            ),
            (
                "android_commandline_tools_version",
                self.android_commandline_tools_version.as_str(),
            ),
            ("android_ndk_version", self.android_ndk_version.as_str()),
            ("chromium_version", self.chromium_version.as_str()),
            ("cmake_version", self.cmake_version.as_str()),
            (
                "expected_xcode_version",
                self.expected_xcode_version.as_str(),
            ),
            ("geckodriver_version", self.geckodriver_version.as_str()),
            ("gitlab_runner_version", self.gitlab_runner_version.as_str()),
            ("gitleaks_version", self.gitleaks_version.as_str()),
            ("linux_android_image", self.linux_android_image.as_str()),
            (
                "linux_android_runner_image",
                self.linux_android_runner_image.as_str(),
            ),
            ("linux_image", self.linux_image.as_str()),
            ("linux_runner_image", self.linux_runner_image.as_str()),
            ("lockbud_rev", self.lockbud_rev.as_str()),
            ("lockbud_toolchain", self.lockbud_toolchain.as_str()),
            ("macos_guest_build", self.macos_guest_build.as_str()),
            ("msrv_toolchain", self.msrv_toolchain.as_str()),
            ("nightly_toolchain", self.nightly_toolchain.as_str()),
            ("rtsan_version", self.rtsan_version.as_str()),
            ("rustup_version", self.rustup_version.as_str()),
            ("stable_toolchain", self.stable_toolchain.as_str()),
            ("windows_eval_iso_url", self.windows_eval_iso_url.as_str()),
        ] {
            if value.trim().is_empty() {
                bail!("CI pin {name} must not be empty");
            }
        }
        if self.android_platform_version == 0 {
            bail!("android_platform_version must be non-zero");
        }
        if self.brew_formulae.is_empty()
            || self.cargo_tools.is_empty()
            || self
                .brew_formulae
                .iter()
                .chain(&self.brew_casks)
                .chain(self.cargo_tools.keys())
                .any(|value| value.trim().is_empty())
            || self
                .cargo_tools
                .values()
                .any(|value| value.trim().is_empty())
        {
            bail!("CI tool package names and versions must not be empty");
        }
        for (name, digest) in [
            (
                "actions_runner_linux_amd64_sha256",
                self.actions_runner_linux_amd64_sha256.as_str(),
            ),
            (
                "actions_runner_linux_arm64_sha256",
                self.actions_runner_linux_arm64_sha256.as_str(),
            ),
            (
                "actions_runner_windows_sha256",
                self.actions_runner_windows_sha256.as_str(),
            ),
            (
                "android_commandline_tools_linux_sha256",
                self.android_commandline_tools_linux_sha256.as_str(),
            ),
            (
                "android_commandline_tools_sha256",
                self.android_commandline_tools_sha256.as_str(),
            ),
            (
                "cmake_linux_arm64_sha256",
                self.cmake_linux_arm64_sha256.as_str(),
            ),
            (
                "cmake_linux_amd64_sha256",
                self.cmake_linux_amd64_sha256.as_str(),
            ),
            (
                "cmake_windows_amd64_sha256",
                self.cmake_windows_amd64_sha256.as_str(),
            ),
            (
                "geckodriver_linux_amd64_sha256",
                self.geckodriver_linux_amd64_sha256.as_str(),
            ),
            ("git_windows_sha256", self.git_windows_sha256.as_str()),
            (
                "geckodriver_linux_arm64_sha256",
                self.geckodriver_linux_arm64_sha256.as_str(),
            ),
            (
                "gitleaks_linux_amd64_sha256",
                self.gitleaks_linux_amd64_sha256.as_str(),
            ),
            (
                "gitleaks_linux_arm64_sha256",
                self.gitleaks_linux_arm64_sha256.as_str(),
            ),
            ("linux_base_digest", self.linux_base_digest.as_str()),
            (
                "rtsan_linux_amd64_sha256",
                self.rtsan_linux_amd64_sha256.as_str(),
            ),
            (
                "rtsan_linux_arm64_sha256",
                self.rtsan_linux_arm64_sha256.as_str(),
            ),
            ("rustup_windows_sha256", self.rustup_windows_sha256.as_str()),
            (
                "windows_eval_iso_sha256",
                self.windows_eval_iso_sha256.as_str(),
            ),
        ] {
            if !is_sha256(digest) {
                bail!("CI pin {name} must be a SHA-256 digest");
            }
        }
        Ok(())
    }

    pub(crate) fn cargo_tool_version(&self, name: &str) -> Result<&str> {
        self.cargo_tools
            .get(name)
            .map(String::as_str)
            .with_context(|| format!("CI pins have no cargo tool named {name}"))
    }

    /// Every role that claims a version pin has to name one this repository
    /// reviews. Without this the two files drift silently: a role keeps
    /// resolving while its pin no longer exists, and the pin guarantees
    /// nothing.
    pub(crate) fn validate_tool_pins(&self, tools: &ToolsConfig) -> Result<()> {
        for (role, pin) in tools.pinned_roles() {
            if !self.cargo_tools.contains_key(pin) {
                bail!(
                    "tool role {role} pins {pin}, which .config/ci-pins.toml \
                     [cargo_tools] does not declare"
                );
            }
        }
        Ok(())
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::config::profile::workspace_root;

    #[test]
    fn digests_are_bounded() {
        assert!(is_sha256(&"a".repeat(64)));
        assert!(!is_sha256(&"a".repeat(63)));
        assert!(!is_sha256(&"z".repeat(64)));
    }

    /// The Quality Lab checks the exact version of every tool it runs, and an
    /// unattended profile turns a missing one into a failure rather than a
    /// skip. The lab and the image declare their tools in different files, so
    /// without this the coverage profile reports a tool error on a machine
    /// nobody changed.
    #[test]
    fn the_image_carries_the_tools_the_coverage_profile_demands() {
        let lab: toml::Value = toml::from_str(
            &fs::read_to_string(workspace_root().join(".config/quality-lab.toml")).unwrap(),
        )
        .unwrap();
        let pins = CiPins::load(&workspace_root().join(".config/ci-pins.toml")).unwrap();

        let demanded = lab["profiles"]["coverage"]["tools"].as_array().unwrap();
        assert!(!demanded.is_empty());
        for tool in demanded {
            let tool = tool.as_str().unwrap();
            let expected = lab["tools"][tool]["version"].as_str().unwrap();
            assert_eq!(pins.cargo_tool_version(tool).unwrap(), expected, "{tool}");
        }
    }

    #[test]
    fn a_role_naming_an_unknown_pin_is_refused() {
        let pins = CiPins::load(&workspace_root().join(".config/ci-pins.toml"))
            .expect("the tracked pins load");
        let tools: ToolsConfig = toml::from_str(
            r#"
            [ast-grep]
            pin = "ast-grepp"
            "#,
        )
        .expect("the tools table parses");

        let error = pins
            .validate_tool_pins(&tools)
            .expect_err("a pin that does not exist must fail at load");

        assert!(format!("{error:#}").contains("ast-grepp"));
    }

    #[test]
    fn the_tracked_tools_table_agrees_with_the_tracked_pins() {
        let root = workspace_root();
        let pins = CiPins::load(&root.join(".config/ci-pins.toml")).expect("pins load");
        let project =
            kithara_devtools::common::project::ProjectConfig::load(root).expect("project loads");

        pins.validate_tool_pins(&project.tools)
            .expect("every configured role names a pin this repository reviews");
    }
}
