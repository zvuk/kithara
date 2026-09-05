use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tracing::info;

use super::runners::path_text;
use crate::ci::{config::CiConfig, process::Process};

pub(super) struct SystemSetup<'a> {
    config: &'a CiConfig,
    process: &'a Process,
}

impl<'a> SystemSetup<'a> {
    pub(super) const fn new(config: &'a CiConfig, process: &'a Process) -> Self {
        Self { config, process }
    }

    pub(super) fn bootstrap(&self) -> Result<()> {
        require_macos()?;
        self.require_root()?;
        self.ensure_volume()?;
        self.ensure_ci_user()?;
        self.ensure_layout()?;
        self.configure_power()?;
        self.install_rosetta()?;
        self.verify_volume()?;
        info!(
            user = self.config.host.ci_user,
            uid = self.config.host.ci_uid,
            root = %self.config.host.host_root.display(),
            quota_bytes = self.config.host.quota_bytes,
            "privileged CI bootstrap completed"
        );
        Ok(())
    }

    pub(super) fn finish(&self) -> Result<()> {
        require_macos()?;
        self.require_root()?;
        let developer = &self.config.host.host_xcode_developer_dir;
        if !developer.is_dir() {
            bail!(
                "install Xcode before finishing CI setup: {}",
                developer.display()
            );
        }
        self.process.run(
            "/usr/bin/xcode-select",
            &["--switch", path_text(developer)?],
            "select Xcode",
        )?;
        let version =
            self.process
                .capture("/usr/bin/xcodebuild", &["-version"], "Xcode version")?;
        let actual = version
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("Xcode "))
            .context("xcodebuild did not return an Xcode version")?;
        if actual != self.config.pins.expected_xcode_version {
            bail!(
                "Xcode {} is required, found {actual}",
                self.config.pins.expected_xcode_version
            );
        }
        self.process.run(
            "/usr/bin/xcodebuild",
            &["-license", "accept"],
            "accept Xcode license",
        )?;
        self.process.run(
            "/usr/bin/xcodebuild",
            &["-runFirstLaunch"],
            "finish Xcode installation",
        )?;
        self.process.run(
            "/usr/sbin/DevToolsSecurity",
            &["-enable"],
            "enable developer mode",
        )?;
        self.process.run(
            "/usr/bin/safaridriver",
            &["--enable"],
            "enable Safari driver",
        )?;
        info!(xcode = actual, "privileged Xcode setup completed");
        Ok(())
    }

    fn require_root(&self) -> Result<()> {
        let uid = self
            .process
            .capture("/usr/bin/id", &["-u"], "current user id")?;
        if uid != "0" {
            bail!("run this command with sudo");
        }
        Ok(())
    }

    fn ensure_volume(&self) -> Result<()> {
        if self.config.host.host_root.is_dir() {
            return Ok(());
        }
        let info = self.process.capture(
            "/usr/sbin/diskutil",
            &["info", "/"],
            "root disk information",
        )?;
        let container = parse_field(&info, "APFS Container")
            .filter(|value| is_disk_identifier(value))
            .context("cannot resolve the root APFS container")?;
        let name = self
            .config
            .host
            .host_root
            .file_name()
            .and_then(|name| name.to_str())
            .context("CI volume path has no UTF-8 volume name")?;
        let quota = format!("{}b", self.config.host.quota_bytes);
        self.process.run(
            "/usr/sbin/diskutil",
            &[
                "apfs",
                "addVolume",
                container,
                "APFS",
                name,
                "-quota",
                &quota,
            ],
            "create CI volume",
        )?;
        self.process.run(
            "/usr/sbin/diskutil",
            &[
                "enableOwnership",
                self.config
                    .host
                    .host_root
                    .to_str()
                    .context("CI volume path is not UTF-8")?,
            ],
            "enable CI volume ownership",
        )
    }

    fn verify_volume(&self) -> Result<()> {
        if !self.config.host.host_root.is_dir() {
            bail!(
                "CI volume is not mounted: {}",
                self.config.host.host_root.display()
            );
        }
        let volume = self
            .config
            .host
            .host_root
            .to_str()
            .context("CI volume path is not UTF-8")?;
        let info =
            self.process
                .capture("/usr/sbin/diskutil", &["info", volume], "CI volume info")?;
        let allocation = parse_first_integer(
            parse_field(&info, "Allocation Block Size")
                .context("CI volume has no allocation block size")?,
        )?;
        let device = parse_field(&info, "Device Identifier")
            .context("CI volume has no device identifier")?;
        let apfs =
            self.process
                .capture("/usr/sbin/diskutil", &["apfs", "list"], "APFS volume list")?;
        let quota =
            parse_volume_quota(&apfs, device).context("CI volume has no APFS capacity quota")?;
        if quota < self.config.host.quota_bytes
            || quota.saturating_sub(self.config.host.quota_bytes) >= allocation
        {
            bail!(
                "CI volume quota must round from {} by less than one {allocation}-byte block, found {quota}",
                self.config.host.quota_bytes
            );
        }
        self.disable_indexing()?;
        self.verify_case_folding()
    }

    /// A mounted volume is indexed by default, and this one holds nothing a
    /// person searches for: it is where every build writes its artifacts. The
    /// indexer then queues each of them, so the cost grows with the build
    /// directory rather than staying constant — the Apple lint job went from
    /// 419s inside a guest, whose disk the host does not index, to 2156s on
    /// this volume with an 8.9 GB `target/`. Applied on every run because it is
    /// the volume's state that matters, not whether this run created it.
    fn disable_indexing(&self) -> Result<()> {
        let host = self.config.host.host_root.as_path();
        let builds = self.config.host.build_root();
        let mut volumes = vec![host];
        if builds != host {
            volumes.push(builds);
        }
        for volume in volumes {
            let path = volume.to_str().context("CI volume path is not UTF-8")?;
            self.process.run(
                "/usr/bin/mdutil",
                &["-i", "off", "-d", path],
                "disable Spotlight indexing on a CI volume",
            )?;
        }
        Ok(())
    }

    /// Apple's own tooling assumes the case folding a stock macOS volume has.
    /// `xcodebuild -create-xcframework` writes `Headers`, and the packaging
    /// step then removes `headers`; on a case-sensitive volume those are two
    /// names and the build dies with `No such file or directory`. This host
    /// was given a case-sensitive volume and `apple:xcframework` never
    /// succeeded on it once, while the same job passed inside a guest whose
    /// disk was an ordinary one.
    fn verify_case_folding(&self) -> Result<()> {
        let probe =
            case_folding_root(self.config).join(format!(".case-check.{}", std::process::id()));
        fs::create_dir(&probe)
            .with_context(|| format!("creating case-folding probe {}", probe.display()))?;
        let lower = probe.join("probe");
        let upper = probe.join("PROBE");
        let result = (|| {
            fs::write(&lower, [])?;
            fs::write(&upper, [])?;
            if fs::read_dir(&probe)?.count() != 1 {
                bail!(
                    "CI volume must fold case like a stock macOS volume; a case-sensitive one \
                     breaks `xcodebuild -create-xcframework` packaging"
                );
            }
            Ok(())
        })();
        let _ = fs::remove_file(lower);
        let _ = fs::remove_file(upper);
        let _ = fs::remove_dir(probe);
        result
    }

    fn ensure_ci_user(&self) -> Result<()> {
        let user = &self.config.host.ci_user;
        let uid = self.config.host.ci_uid.to_string();
        let home = self.ci_home();
        let uid_owner = self
            .process
            .capture(
                "/usr/bin/dscl",
                &[".", "-search", "/Users", "UniqueID", &uid],
                "lookup CI UID",
            )
            .unwrap_or_default();
        if let Some(owner) = uid_owner.split_whitespace().next()
            && owner != user
        {
            bail!("UID {} already belongs to {owner}", self.config.host.ci_uid);
        }
        let exists = self
            .process
            .command("/usr/bin/id")
            .arg(user)
            .status()
            .is_ok_and(|status| status.success());
        if !exists {
            let home = home.to_str().context("CI home path is not UTF-8")?;
            let mut command = self.process.command("/usr/sbin/sysadminctl");
            command.args([
                "-addUser",
                user,
                "-fullName",
                "Kithara CI",
                "-UID",
                &uid,
                "-GID",
                "20",
                "-home",
                home,
                "-shell",
                "/bin/zsh",
                "-password",
                "-",
            ]);
            self.process
                .run_command(&mut command, "create CI user with prompted password")?;
        }
        let actual_uid = self
            .process
            .capture("/usr/bin/id", &["-u", user], "verify CI user id")?;
        if actual_uid != uid {
            bail!("{user} has UID {actual_uid}, expected {uid}");
        }
        let home_record = self.process.capture(
            "/usr/bin/dscl",
            &[".", "-read", &format!("/Users/{user}"), "NFSHomeDirectory"],
            "verify CI user home",
        )?;
        if home_record.split_whitespace().last() != home.to_str() {
            bail!("{user} home does not match {}", home.display());
        }
        self.process.run(
            "/usr/sbin/createhomedir",
            &["-c", "-u", user],
            "create CI home",
        )?;
        self.process.run(
            "/usr/sbin/dseditgroup",
            &["-o", "edit", "-a", user, "-t", "user", "_developer"],
            "grant developer access",
        )?;
        self.process.run(
            "/usr/sbin/dseditgroup",
            &[
                "-o",
                "edit",
                "-a",
                user,
                "-t",
                "user",
                "com.apple.access_ssh",
            ],
            "grant SSH access",
        )
    }

    fn ensure_layout(&self) -> Result<()> {
        for name in [
            "cache",
            "logs",
            "toolchains",
            "vm",
            "workspaces",
            "workspaces/tmp",
        ] {
            self.install_directory(
                &self.config.host.host_root.join(name),
                "0750",
                &self.config.host.ci_user,
            )?;
        }
        for directory in checkout_directories(self.config) {
            self.install_directory(&directory, "0750", &self.config.host.ci_user)?;
        }
        self.install_directory(&self.config.host.host_root.join("services"), "0755", "root")?;
        self.install_directory(
            &self.ci_home().join(".ssh"),
            "0700",
            &self.config.host.ci_user,
        )?;
        let admin_record = self.process.capture(
            "/usr/bin/dscl",
            &[
                ".",
                "-read",
                &format!("/Users/{}", self.config.host.admin_user),
                "NFSHomeDirectory",
            ],
            "read administrator home",
        )?;
        let admin_home = admin_record
            .split_whitespace()
            .last()
            .map(PathBuf::from)
            .context("administrator account has no home directory")?;
        let authorized = admin_home.join(".ssh/authorized_keys");
        if !authorized.is_file() {
            bail!("missing {}", authorized.display());
        }
        let target = self.ci_home().join(".ssh/authorized_keys");
        self.install_file(&authorized, &target, "0600", &self.config.host.ci_user)
    }

    fn configure_power(&self) -> Result<()> {
        self.process.run(
            "/usr/bin/pmset",
            &[
                "-a",
                "autorestart",
                "1",
                "disksleep",
                "0",
                "displaysleep",
                "10",
                "powernap",
                "0",
                "sleep",
                "0",
                "tcpkeepalive",
                "1",
                "womp",
                "1",
            ],
            "configure CI power policy",
        )?;
        self.process.run(
            "/usr/sbin/sysadminctl",
            &["-automaticTime", "on"],
            "enable automatic time",
        )?;
        self.process.run(
            "/usr/sbin/sysadminctl",
            &["-guestAccount", "off"],
            "disable guest account",
        )?;
        let auto_user = self
            .process
            .capture(
                "/usr/bin/defaults",
                &[
                    "read",
                    "/Library/Preferences/com.apple.loginwindow",
                    "autoLoginUser",
                ],
                "automatic login user",
            )
            .unwrap_or_default();
        if auto_user != self.config.host.ci_user || !Path::new("/etc/kcpassword").is_file() {
            bail!(
                "enable automatic login for {} in System Settings, then rerun",
                self.config.host.ci_user
            );
        }
        Ok(())
    }

    fn install_rosetta(&self) -> Result<()> {
        let available = self
            .process
            .command("/usr/bin/arch")
            .args(["-x86_64", "/usr/bin/true"])
            .status()
            .is_ok_and(|status| status.success());
        if available {
            return Ok(());
        }
        self.process.run(
            "/usr/sbin/softwareupdate",
            &["--install-rosetta", "--agree-to-license"],
            "install Rosetta",
        )
    }

    fn ci_home(&self) -> PathBuf {
        self.config
            .host
            .host_root
            .join("home")
            .join(&self.config.host.ci_user)
    }

    fn install_directory(&self, path: &Path, mode: &str, owner: &str) -> Result<()> {
        let path = path.to_str().context("install path is not UTF-8")?;
        self.process.run(
            "/usr/bin/install",
            &["-d", "-m", mode, "-o", owner, "-g", "staff", path],
            "install CI directory",
        )
    }

    fn install_file(&self, source: &Path, target: &Path, mode: &str, owner: &str) -> Result<()> {
        self.process.run(
            "/usr/bin/install",
            &[
                "-m",
                mode,
                "-o",
                owner,
                "-g",
                "staff",
                source.to_str().context("source path is not UTF-8")?,
                target.to_str().context("target path is not UTF-8")?,
            ],
            "install CI file",
        )
    }
}

fn case_folding_root(config: &CiConfig) -> &Path {
    config.host.build_root()
}

fn checkout_directories(config: &CiConfig) -> Vec<PathBuf> {
    let build_root = config.host.build_root();
    let mut directories = Vec::with_capacity(3);
    if build_root != config.host.host_root {
        directories.push(build_root.to_path_buf());
    }
    directories.push(build_root.join("workspaces"));
    directories.push(build_root.join("workspaces/gitlab"));
    directories
}

fn require_macos() -> Result<()> {
    if std::env::consts::OS != "macos" {
        bail!("this host command supports macOS only");
    }
    Ok(())
}

fn parse_field<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (name, value) = line.trim().split_once(':')?;
        (name.trim() == field).then_some(value.trim())
    })
}

fn parse_first_integer(text: &str) -> Result<u64> {
    text.split_whitespace()
        .find(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
        .context("field contains no integer")?
        .parse()
        .context("field integer is out of range")
}

fn is_disk_identifier(value: &str) -> bool {
    value.strip_prefix("disk").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// `diskutil apfs list` draws containers as a tree, so every field line is
/// prefixed with one or more `|` guides that `str::trim` leaves in place.
fn strip_tree_guides(line: &str) -> &str {
    line.trim_start_matches(|character: char| character == '|' || character.is_whitespace())
        .trim_end()
}

fn parse_volume_quota(text: &str, device: &str) -> Option<u64> {
    let header = format!("+-> Volume {device} ");
    let mut in_volume = false;
    for line in text.lines() {
        let entry = strip_tree_guides(line);
        if entry.starts_with("+-> Volume ") {
            if in_volume {
                return None;
            }
            in_volume = entry.starts_with(&header);
            continue;
        }
        if in_volume && entry.starts_with("Capacity Quota:") {
            return parse_first_integer(entry).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_container_parser_accepts_current_diskutil_shape() {
        let text = "   APFS Container:             disk3\n";
        assert_eq!(parse_field(text, "APFS Container"), Some("disk3"));
        assert!(is_disk_identifier("disk3"));
        assert!(!is_disk_identifier("disk3s1"));
    }

    /// Shape produced by `diskutil apfs list` on the CI host: every field line
    /// carries the container's `|` tree guides.
    const APFS_LIST: &str = "\
|   +-> Volume disk3s6 4E1F0D1A-0000-0000-0000-000000000000\n\
|   |   ---------------------------------------------------\n\
|   |   APFS Volume Disk (Role):   disk3s6 (VM)\n\
|   |   Name:                      VM (Case-insensitive)\n\
|   |   Capacity Consumed:         2147504128 B (2.1 GB)\n\
|   |\n\
|   +-> Volume disk3s7 07077B7C-0000-0000-0000-000000000000\n\
|       ---------------------------------------------------\n\
|       APFS Volume Disk (Role):   disk3s7 (No specific role)\n\
|       Name:                      KitharaCI (Case-sensitive)\n\
|       Mount Point:               /Volumes/KitharaCI\n\
|       Capacity Consumed:         28917514240 B (28.9 GB)\n\
|       Capacity Reserve:          None\n\
|       Capacity Quota:            300000002048 B (300.0 GB) (9.6% reached)\n\
|       Sealed:                    No\n";

    #[test]
    fn quota_parser_reads_the_tree_shape_diskutil_emits() {
        assert_eq!(
            parse_volume_quota(APFS_LIST, "disk3s7"),
            Some(300_000_002_048)
        );
    }

    #[test]
    fn quota_parser_is_scoped_to_device() {
        assert_eq!(parse_volume_quota(APFS_LIST, "disk3s6"), None);
        assert_eq!(parse_volume_quota(APFS_LIST, "disk9s1"), None);
    }

    #[test]
    fn case_folding_belongs_to_the_checkout_root() {
        let mut config = crate::ci::config::fixture();
        config.host.host_root = PathBuf::from("/case-sensitive-ci-root");
        config.host.build_root = Some(PathBuf::from("/case-folding-build-root"));

        assert_eq!(
            case_folding_root(&config),
            Path::new("/case-folding-build-root")
        );
    }

    /// One tool, one address. The privileged bootstrap spells `xcodebuild`
    /// absolutely because it runs as root, where `PATH` must not decide. The
    /// lane reads the same Xcode's version through the config, so every
    /// spelling here has to be the one the config answers with — otherwise a
    /// lane can pass a version check the host never made.
    #[test]
    fn the_bootstrap_spells_xcodebuild_the_way_the_config_resolves_it() {
        let root = crate::ci::config::workspace_root();
        let project = kithara_devtools::common::project::ProjectConfig::load(root)
            .expect("the project config loads");
        let source = fs::read_to_string(root.join("xtask/src/ci/host/system.rs"))
            .expect("this source is readable");
        let (bootstrap, _) = source
            .split_once("\n#[cfg(test)]")
            .expect("this source carries a test module to cut the production half at");

        let configured = project.tools.program("xcodebuild");
        let spellings: Vec<&str> = bootstrap
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|literal| literal.ends_with("xcodebuild"))
            .collect();

        assert!(!spellings.is_empty(), "the bootstrap spawns xcodebuild");
        for spelling in spellings {
            assert_eq!(
                spelling, configured,
                "the bootstrap spells xcodebuild differently from what the \
                 config resolves to"
            );
        }
    }

    #[test]
    fn layout_prepares_the_selected_checkout_root() {
        let mut config = crate::ci::config::fixture();
        config.host.host_root = PathBuf::from("/host-root");
        config.host.build_root = Some(PathBuf::from("/build-root"));

        assert_eq!(
            checkout_directories(&config),
            [
                "/build-root",
                "/build-root/workspaces",
                "/build-root/workspaces/gitlab",
            ]
            .map(PathBuf::from)
        );
    }
}
