use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use super::runners::{RunnerManager, path_text, require_macos};
use crate::ci::config::{LANE_CONFIG_DIR, MAC_CONFIG_PATH};

/// Where the guest reaches the shared directories.
///
/// virtiofs auto-mounts them under `/Volumes/My Shared Files`, and GNU make
/// cannot represent a path containing spaces at all — space is its separator
/// between targets, with no escape. `xcrun` resolves symlinks before handing
/// the toolchain path to cmake, cmake writes it into the generated Makefile,
/// and make then reports `/Volumes/My: No such file or directory`. Only a real
/// mount elsewhere fixes it, so the guest moves the share here on startup.
pub(super) const GUEST_SHARE: &str = "/opt/kithara";

pub(super) fn guest_developer_dir() -> String {
    format!("{GUEST_SHARE}/Xcode.app/Contents/Developer")
}

impl RunnerManager<'_> {
    pub(super) fn prepare_guest(&self) -> Result<()> {
        require_macos()?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("guest HOME is not set")?;
        let shared = Path::new(GUEST_SHARE);
        for (name, source) in [
            (".cargo", shared.join("kithara-cargo")),
            (".rustup", shared.join("kithara-rustup")),
        ] {
            let target = home.join(name);
            remove_guest_path(&target)?;
            create_guest_symlink(&source, &target)?;
        }
        // Homebrew bakes its prefix into every dylib install name, so the
        // shared copy only resolves when the guest reaches it at that path.
        let brew = &self.config.host.brew_root;
        let brew_parent = brew
            .parent()
            .context("brew_root has no parent directory to create in the guest")?;
        sudo(
            &["install", "-d", "-m", "0755", path_text(brew_parent)?],
            "create guest Homebrew parent directory",
        )?;
        sudo(
            &[
                "ln",
                "-sfn",
                &format!("{GUEST_SHARE}/kithara-brew"),
                path_text(brew)?,
            ],
            "link guest Homebrew prefix",
        )?;
        sudo(
            &["xcode-select", "-s", &guest_developer_dir()],
            "select guest Xcode",
        )?;
        // Only `Xcode.app` is shared; the system content it installs on first
        // launch lives outside the bundle and a fresh guest has none of it.
        // Without it `xcodebuild` cannot load `IDESimulatorFoundation`, which
        // it needs for the simulator slices of the XCFramework.
        sudo(
            &["/usr/bin/xcodebuild", "-runFirstLaunch"],
            "install guest Xcode system content",
        )?;
        // The host enables both of these once, when it is provisioned. The
        // guest is a fresh clone of a plain macOS install every time and
        // inherits neither: without developer mode the debugger cannot attach
        // to a test process, and without this Safari refuses to be driven, so
        // the browser lane has nothing to talk to.
        sudo(
            &["/usr/sbin/DevToolsSecurity", "-enable"],
            "enable guest developer mode",
        )?;
        sudo(
            &["/usr/bin/safaridriver", "--enable"],
            "enable guest Safari driver",
        )?;
        // `safaridriver --enable` is the machine half. Safari itself will not
        // pair with an automation session until the account it runs under also
        // asks for it, and a fresh guest has never opened Safari: the driver
        // started and answered `ready`, then every session died on "Request to
        // pair with an automation session" timing out.
        for key in ["IncludeDevelopMenu", "AllowRemoteAutomation"] {
            plain(
                &[
                    "/usr/bin/defaults",
                    "write",
                    "com.apple.Safari",
                    key,
                    "-bool",
                    "true",
                ],
                "allow guest Safari automation",
            )?;
        }
        // A stock install sleeps after a minute. A browser lane is minutes of
        // waiting on a driver, and a suite is longer still.
        sudo(
            &[
                "/usr/bin/pmset",
                "-a",
                "sleep",
                "0",
                "displaysleep",
                "0",
                "disablesleep",
                "1",
            ],
            "keep the guest awake",
        )?;
        // A freshly installed macOS has no /usr/local/bin, so `install` into it
        // fails until the directory exists.
        sudo(
            &["install", "-d", "-m", "0755", "/usr/local/bin"],
            "create guest local bin directory",
        )?;
        sudo(
            &[
                "install",
                "-m",
                "0755",
                path_text(&shared.join("kithara-tools/xcodegen"))?,
                "/usr/local/bin/xcodegen",
            ],
            "install guest xcodegen",
        )?;
        sudo(
            &["install", "-d", "-m", "0755", LANE_CONFIG_DIR],
            "create guest lane configuration directory",
        )?;
        sudo(
            &[
                "install",
                "-m",
                "0644",
                path_text(&shared.join("kithara-tools/mac-host.toml"))?,
                MAC_CONFIG_PATH,
            ],
            "install guest host profile",
        )?;
        let xcode =
            self.process
                .capture("/usr/bin/xcodebuild", &["-version"], "guest Xcode version")?;
        if !xcode.starts_with(&format!(
            "Xcode {}\n",
            self.config.pins.expected_xcode_version
        )) {
            bail!(
                "guest Xcode does not match {}",
                self.config.pins.expected_xcode_version
            );
        }
        // GNU make cannot express a path containing a space, and cmake feeds
        // it whatever `xcrun` resolves the toolchain to. Assert the property
        // the build depends on, so a regression fails here and says why,
        // instead of surfacing two minutes later inside a compiler probe.
        let make =
            self.process
                .capture("/usr/bin/xcrun", &["--find", "make"], "guest make path")?;
        if make.contains(char::is_whitespace) {
            bail!("guest toolchain path contains whitespace, which make cannot use: {make}");
        }
        self.process.require_tools(&[
            "cargo",
            "cmake",
            "ffmpeg",
            "just",
            "pkg-config",
            "sccache",
            "xcodegen",
        ])?;
        Ok(())
    }
}

fn remove_guest_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("removing {}", path.display()))
    }
}

#[cfg(unix)]
fn create_guest_symlink(source: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(source, target).with_context(|| {
        format!(
            "linking guest {} to shared {}",
            target.display(),
            source.display()
        )
    })
}

#[cfg(not(unix))]
fn create_guest_symlink(_source: &Path, _target: &Path) -> Result<()> {
    bail!("macOS guest preparation requires Unix symlinks")
}

/// Runs as the guest account rather than through `sudo`. Per-user settings
/// written as root land in root's own domain and the account never sees them.
fn plain(args: &[&str], label: &str) -> Result<()> {
    let (program, rest) = args.split_first().context("empty guest command")?;
    let status = std::process::Command::new(program)
        .args(rest)
        .status()
        .with_context(|| format!("starting {label}"))?;
    if !status.success() {
        bail!(
            "{label} failed with exit code {}",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

fn sudo(args: &[&str], label: &str) -> Result<()> {
    let status = std::process::Command::new("/usr/bin/sudo")
        .arg("-n")
        .args(args)
        .status()
        .with_context(|| format!("starting {label}"))?;
    if !status.success() {
        bail!(
            "{label} failed with exit code {}",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}
