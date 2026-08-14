use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tracing::info;

use crate::ci::{
    bridge,
    config::{CiConfig, LANE_CONFIG_DIR, MAC_CONFIG_PATH},
    process::Process,
};

pub(super) struct ServiceInstaller<'a> {
    config: &'a CiConfig,
    process: &'a Process,
}

impl<'a> ServiceInstaller<'a> {
    const BINARY_NAME: &'static str = "kithara-ci";
    const HOST_CONFIG_NAME: &'static str = "mac-host.toml";
    const PINS_NAME: &'static str = "pins.toml";

    pub(super) const fn new(config: &'a CiConfig, process: &'a Process) -> Self {
        Self { config, process }
    }

    pub(super) fn install(&self, config_path: &Path, pins_path: &Path) -> Result<()> {
        require_macos()?;
        self.require_root()?;
        self.ensure_sync_user()?;
        self.ensure_layout()?;
        self.install_binary()?;
        self.install_config(config_path, pins_path)?;
        self.install_maintenance_agents()?;
        self.stage_bridge_agent()?;
        info!(
            binary = %self.installed_binary().display(),
            "root-owned CI services installed"
        );
        Ok(())
    }

    pub(super) fn activate_bridge(&self) -> Result<()> {
        require_macos()?;
        self.require_root()?;
        let bridge = self.service_root().join("bridge");
        let config = bridge.join("config.toml");
        let pending = bridge.join("com.zvuk.kithara-ci.bridge.plist.pending");
        if !config.is_file() {
            bail!("missing bridge configuration: {}", config.display());
        }
        if !pending.is_file() {
            bail!("missing staged bridge service: {}", pending.display());
        }
        self.require_private_sync_file(&config)?;
        for secret in bridge::secret_files(&config)? {
            self.require_private_sync_file(&secret)?;
        }
        let binary = self.installed_binary();
        self.process.run(
            path_text(&binary)?,
            &["ci", "bridge", "validate", "--config", path_text(&config)?],
            "validate repository bridge configuration",
        )?;
        let destination = Path::new("/Library/LaunchDaemons/com.zvuk.kithara-ci.bridge.plist");
        self.install_file(&pending, destination, "0644", "root")?;
        let service = "system/com.zvuk.kithara-ci.bridge";
        let _ = self
            .process
            .command("/bin/launchctl")
            .args(["bootout", service])
            .status();
        self.process.run(
            "/bin/launchctl",
            &["bootstrap", "system", path_text(destination)?],
            "activate repository bridge",
        )?;
        self.process.run(
            "/bin/launchctl",
            &["enable", service],
            "enable repository bridge",
        )?;
        self.process.run(
            "/bin/launchctl",
            &["kickstart", "-k", service],
            "start repository bridge",
        )?;
        info!("repository bridge activated");
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

    fn ensure_sync_user(&self) -> Result<()> {
        let user = &self.config.host.sync_user;
        let uid = self.config.host.sync_uid.to_string();
        let home = self.sync_home();
        let owner = self
            .process
            .capture(
                "/usr/bin/dscl",
                &[".", "-search", "/Users", "UniqueID", &uid],
                "lookup synchronization UID",
            )
            .unwrap_or_default();
        if let Some(existing) = owner.split_whitespace().next()
            && existing != user
        {
            bail!(
                "UID {} already belongs to {existing}",
                self.config.host.sync_uid
            );
        }
        let exists = self
            .process
            .command("/usr/bin/id")
            .arg(user)
            .status()
            .is_ok_and(|status| status.success());
        if !exists {
            let record = format!("/Users/{user}");
            let home = home.to_str().context("synchronization home is not UTF-8")?;
            for args in [
                vec![".", "-create", &record],
                vec![
                    ".",
                    "-create",
                    &record,
                    "RealName",
                    "Kithara Repository Sync",
                ],
                vec![".", "-create", &record, "UniqueID", &uid],
                vec![".", "-create", &record, "PrimaryGroupID", "20"],
                vec![".", "-create", &record, "NFSHomeDirectory", home],
                vec![".", "-create", &record, "UserShell", "/usr/bin/false"],
                vec![".", "-create", &record, "IsHidden", "1"],
            ] {
                let mut command = self.process.command("/usr/bin/dscl");
                command.args(args);
                self.process
                    .run_command(&mut command, "create synchronization user")?;
            }
        }
        let actual_uid =
            self.process
                .capture("/usr/bin/id", &["-u", user], "verify synchronization user")?;
        if actual_uid != uid {
            bail!("{user} has UID {actual_uid}, expected {uid}");
        }
        Ok(())
    }

    fn ensure_layout(&self) -> Result<()> {
        for (path, mode, owner) in [
            (self.service_root().join("bin"), "0755", "root"),
            (self.service_root().join("bridge"), "0755", "root"),
            (
                self.service_root().join("bridge/secrets"),
                "0700",
                self.config.host.sync_user.as_str(),
            ),
            (
                self.service_root().join("bridge/state"),
                "0700",
                self.config.host.sync_user.as_str(),
            ),
            (
                self.sync_home(),
                "0700",
                self.config.host.sync_user.as_str(),
            ),
            (self.agent_root(), "0755", self.config.host.ci_user.as_str()),
        ] {
            self.install_directory(&path, mode, owner)?;
        }
        let log = self.config.host.host_root.join("logs/bridge.log");
        if !log.exists() {
            self.install_empty_file(&log, "0640", &self.config.host.sync_user)?;
        }
        Ok(())
    }

    fn install_binary(&self) -> Result<()> {
        let source = std::env::current_exe().context("resolving current CI executable")?;
        self.install_file(&source, &self.installed_binary(), "0755", "root")?;
        let shared = self.config.host.host_root.join("toolchains/shared-bin");
        self.install_directory(&shared, "0755", &self.config.host.ci_user)?;
        self.install_file(
            &source,
            &shared.join(Self::BINARY_NAME),
            "0755",
            &self.config.host.ci_user,
        )
    }

    fn install_config(&self, config_path: &Path, pins_path: &Path) -> Result<()> {
        for (label, path) in [
            ("CI host profile", config_path),
            ("CI build pins", pins_path),
        ] {
            if !path.is_file() {
                bail!("missing {label}: {}", path.display());
            }
        }
        let host = self.installed_config();
        let pins = self.installed_pins();
        self.config.host.write(&host)?;
        self.config.pins.write(&pins)?;
        let shared = self.config.host.host_root.join("toolchains/shared-bin");
        for (source, name) in [(&host, Self::HOST_CONFIG_NAME), (&pins, Self::PINS_NAME)] {
            self.process.run(
                "/usr/sbin/chown",
                &["root:wheel", path_text(source)?],
                "set installed CI config ownership",
            )?;
            self.process.run(
                "/bin/chmod",
                &["0644", path_text(source)?],
                "set installed CI config permissions",
            )?;
            self.install_file(
                source,
                &shared.join(name),
                "0644",
                &self.config.host.ci_user,
            )?;
        }
        self.install_directory(Path::new(LANE_CONFIG_DIR), "0755", "root")?;
        self.install_file(&host, Path::new(MAC_CONFIG_PATH), "0644", "root")
    }

    fn install_maintenance_agents(&self) -> Result<()> {
        let binary = self.replaceable_binary().display().to_string();
        let config = self.installed_config().display().to_string();
        let pins = self.installed_pins().display().to_string();
        let logs = &self.config.host.host_root.join("logs");
        let path = self.config.host.agent_path(&self.ci_home());
        let cleanup = launchd(
            "com.zvuk.kithara-ci.cleanup",
            &[
                &binary, "ci", "host", "--config", &config, "--pins", &pins, "cleanup",
            ],
            &logs.join("cleanup.log"),
            &path,
            // Hourly is slower than the host spends. Under a build this volume
            // loses about 2.7 GB a minute — measured dropping from 53 to 31 GB
            // free in eight minutes — so the 45 GB between the first cleanup
            // step and refusing jobs is a quarter of an hour. A pass that comes
            // once an hour arrives after the refusals it exists to prevent.
            "<key>StartInterval</key><integer>900</integer>",
        );
        let health = launchd(
            "com.zvuk.kithara-ci.health",
            &[
                &binary, "ci", "host", "--config", &config, "--pins", &pins, "health",
            ],
            &logs.join("health.log"),
            &path,
            "<key>StartInterval</key><integer>300</integer>",
        );
        self.write_agent("cleanup", &cleanup)?;
        self.write_agent("health", &health)
    }

    /// The daemon runs the copy under `toolchains/shared-bin`, the one the CI
    /// user can replace, for the same reason the maintenance agents do: every
    /// fix to the bridge otherwise needs a password, and the bridge is the part
    /// that gets fixed while it is being brought up. The secrets stay out of
    /// reach — they belong to the sync user, and the CI user cannot read them.
    fn stage_bridge_agent(&self) -> Result<()> {
        let binary = self
            .config
            .host
            .host_root
            .join("toolchains/shared-bin")
            .join(Self::BINARY_NAME)
            .display()
            .to_string();
        let config = self
            .service_root()
            .join("bridge/config.toml")
            .display()
            .to_string();
        let log = self.config.host.host_root.join("logs/bridge.log");
        let plist = bridge_launchd(
            &binary,
            &config,
            &log,
            &self.config.host.agent_path(&self.sync_home()),
            &self.config.host.sync_user,
        );
        let pending = self
            .service_root()
            .join("bridge/com.zvuk.kithara-ci.bridge.plist.pending");
        fs::write(&pending, plist)
            .with_context(|| format!("writing pending bridge service {}", pending.display()))?;
        self.process.run(
            "/usr/sbin/chown",
            &["root:wheel", path_text(&pending)?],
            "set bridge service ownership",
        )
    }

    #[cfg(unix)]
    fn require_private_sync_file(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
        if metadata.uid() != self.config.host.sync_uid {
            bail!(
                "{} must be owned by UID {}",
                path.display(),
                self.config.host.sync_uid
            );
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "{} must not be accessible by group or others",
                path.display()
            );
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn require_private_sync_file(&self, _path: &Path) -> Result<()> {
        bail!("bridge activation requires Unix ownership metadata")
    }

    fn write_agent(&self, name: &str, contents: &str) -> Result<()> {
        let path = self
            .agent_root()
            .join(format!("com.zvuk.kithara-ci.{name}.plist"));
        fs::write(&path, contents)
            .with_context(|| format!("writing launch agent {}", path.display()))?;
        self.process.run(
            "/usr/sbin/chown",
            &[
                &format!("{}:staff", self.config.host.ci_user),
                path_text(&path)?,
            ],
            "set launch agent ownership",
        )?;
        self.process.run(
            "/bin/chmod",
            &["0644", path_text(&path)?],
            "set launch agent permissions",
        )
    }

    fn install_directory(&self, path: &Path, mode: &str, owner: &str) -> Result<()> {
        self.process.run(
            "/usr/bin/install",
            &[
                "-d",
                "-m",
                mode,
                "-o",
                owner,
                "-g",
                if owner == "root" { "wheel" } else { "staff" },
                path_text(path)?,
            ],
            "install service directory",
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
                if owner == "root" { "wheel" } else { "staff" },
                path_text(source)?,
                path_text(target)?,
            ],
            "install service file",
        )
    }

    fn install_empty_file(&self, target: &Path, mode: &str, owner: &str) -> Result<()> {
        self.install_file(Path::new("/dev/null"), target, mode, owner)
    }

    fn service_root(&self) -> PathBuf {
        self.config.host.host_root.join("services")
    }

    fn installed_binary(&self) -> PathBuf {
        self.service_root().join("bin").join(Self::BINARY_NAME)
    }

    /// The copy the CI user can replace, which is what the periodic agents run.
    ///
    /// `services/bin` is `root:wheel`, so a fix to cleanup or health reaches
    /// the host only through an operator with a password. That is how a broken
    /// cleanup stayed broken: the repository had the fix and the mac kept
    /// running the binary from before it. The bridge daemon already runs from
    /// here for exactly this reason.
    fn replaceable_binary(&self) -> PathBuf {
        self.config
            .host
            .host_root
            .join("toolchains/shared-bin")
            .join(Self::BINARY_NAME)
    }

    fn installed_config(&self) -> PathBuf {
        self.service_root().join(Self::HOST_CONFIG_NAME)
    }

    fn installed_pins(&self) -> PathBuf {
        self.service_root().join(Self::PINS_NAME)
    }

    fn ci_home(&self) -> PathBuf {
        self.config
            .host
            .host_root
            .join("home")
            .join(&self.config.host.ci_user)
    }

    fn sync_home(&self) -> PathBuf {
        self.config
            .host
            .host_root
            .join("home")
            .join(&self.config.host.sync_user)
    }

    fn agent_root(&self) -> PathBuf {
        self.ci_home().join("Library/LaunchAgents")
    }
}

fn require_macos() -> Result<()> {
    if std::env::consts::OS != "macos" {
        bail!("service installation supports macOS only");
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
}

pub(super) fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn launchd(
    label: &str,
    arguments: &[&str],
    log: &Path,
    path: &str,
    extra: &str,
) -> String {
    let arguments = arguments
        .iter()
        .map(|argument| format!("<string>{}</string>", xml(argument)))
        .collect::<String>();
    let log = xml(&log.display().to_string());
    let path = xml(path);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\
         <key>EnvironmentVariables</key><dict>\
         <key>PATH</key><string>{path}</string>\
         </dict>\
         <key>Label</key><string>{label}</string>\
         <key>ProgramArguments</key><array>{arguments}</array>\
         <key>RunAtLoad</key><true/>{extra}\
         <key>StandardErrorPath</key><string>{log}</string>\
         <key>StandardOutPath</key><string>{log}</string>\
         </dict></plist>\n"
    )
}

fn bridge_launchd(binary: &str, config: &str, log: &Path, path: &str, user: &str) -> String {
    let extra = format!(
        "<key>StartInterval</key><integer>60</integer><key>UserName</key><string>{}</string>",
        xml(user)
    );
    launchd(
        "com.zvuk.kithara-ci.bridge",
        &[
            "/usr/bin/env",
            binary,
            "ci",
            "bridge",
            "reconcile",
            "--config",
            config,
        ],
        log,
        path,
        &extra,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ci::config::fixture;

    #[test]
    fn generated_agent_escapes_paths() {
        let plist = launchd(
            "test",
            &["/a&b", "ci"],
            Path::new("/tmp/a&b.log"),
            "/opt/homebrew/bin:/usr/bin",
            "",
        );
        assert!(plist.contains("<string>/a&amp;b</string>"));
        assert!(plist.contains("<string>/tmp/a&amp;b.log</string>"));
    }

    /// `launchd` hands agents a minimal PATH, so an agent that shells out to a
    /// Homebrew tool (colima -> limactl) dies unless PATH is spelled out.
    #[test]
    fn generated_agent_declares_a_path() {
        let plist = launchd(
            "test",
            &["/opt/homebrew/bin/colima"],
            Path::new("/tmp/a.log"),
            "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/bin:/bin",
            "",
        );
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains(
            "<key>PATH</key><string>/opt/homebrew/bin:/opt/homebrew/sbin:/usr/bin:/bin</string>"
        ));
    }

    /// The periodic agents ran `services/bin`, which is `root:wheel`. Every
    /// fix to cleanup then needed an operator with a password, and the one
    /// that did not arrive is why this host refused jobs for a week.
    #[test]
    fn the_periodic_agents_run_the_binary_the_ci_user_can_replace() {
        let mut config = fixture();
        config.host.host_root = PathBuf::from("/Volumes/CI");
        let process = Process::new(Path::new("/Volumes/CI"), BTreeMap::new());
        let services = ServiceInstaller::new(&config, &process);

        assert_eq!(
            services.replaceable_binary(),
            PathBuf::from("/Volumes/CI/toolchains/shared-bin/kithara-ci")
        );
    }

    #[test]
    fn bridge_agent_execs_mutable_binary_through_stable_env() {
        let plist = bridge_launchd(
            "/Volumes/CI/toolchains/shared-bin/kithara-ci",
            "/Volumes/CI/services/bridge/config.toml",
            Path::new("/Volumes/CI/logs/bridge.log"),
            "/opt/homebrew/bin:/usr/bin:/bin",
            "kithara-sync",
        );

        assert!(plist.contains(
            "<key>ProgramArguments</key><array>\
             <string>/usr/bin/env</string>\
             <string>/Volumes/CI/toolchains/shared-bin/kithara-ci</string>\
             <string>ci</string><string>bridge</string><string>reconcile</string>\
             <string>--config</string>\
             <string>/Volumes/CI/services/bridge/config.toml</string></array>"
        ));
        assert!(!plist.contains("<string>/bin/sh</string>"));
        assert!(!plist.contains("<string>-c</string>"));
    }
}
