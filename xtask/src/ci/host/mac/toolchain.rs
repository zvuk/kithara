use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use tracing::info;

use crate::ci::{config::CiConfig, process::Process};

pub(super) struct ToolchainInstaller<'a> {
    config: &'a CiConfig,
    process: &'a Process,
}

impl<'a> ToolchainInstaller<'a> {
    pub(super) const fn new(config: &'a CiConfig, process: &'a Process) -> Self {
        Self { config, process }
    }

    pub(super) fn install_host_tools(&self) -> Result<()> {
        require_macos()?;
        self.require_user(&self.config.host.admin_user)?;
        let brew = self.config.host.brew_tool("brew");
        if !brew.is_file() {
            bail!("Homebrew is not installed at {}", brew.display());
        }
        let mut analytics = self.process.command(&brew);
        analytics
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .arg("analytics")
            .arg("off");
        self.process
            .run_command(&mut analytics, "disable Homebrew analytics")?;
        let mut install = self.process.command(&brew);
        install
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .arg("install")
            .args(&self.config.pins.brew_formulae);
        self.process
            .run_command(&mut install, "install CI host formulae")?;
        for cask in &self.config.pins.brew_casks {
            let installed = self
                .process
                .command(&brew)
                .args(["list", "--cask", cask])
                .status()
                .is_ok_and(|status| status.success());
            if !installed {
                let mut command = self.process.command(&brew);
                command
                    .env("HOMEBREW_NO_AUTO_UPDATE", "1")
                    .args(["install", "--cask", cask]);
                self.process
                    .run_command(&mut command, "install CI host cask")?;
            }
        }
        let encoder = self
            .process
            .command(self.config.host.brew_tool("mac"))
            .output()
            .context("Monkey's Audio version")?;
        let banner = String::from_utf8_lossy(&encoder.stdout);
        if !banner.contains(&format!("v {}", self.config.pins.monkeys_audio_version)) {
            bail!(
                "Monkey's Audio {} is required, found {banner}",
                self.config.pins.monkeys_audio_version
            );
        }
        let runner_path = self.config.host.brew_tool("gitlab-runner");
        let runner = self.process.capture(
            path_text(&runner_path)?,
            &["--version"],
            "GitLab Runner version",
        )?;
        if !runner.contains(&self.config.pins.gitlab_runner_version) {
            bail!(
                "GitLab Runner {} is required, found {runner}",
                self.config.pins.gitlab_runner_version
            );
        }
        info!("CI host tools installed");
        Ok(())
    }

    pub(super) fn install_user_tools(&self) -> Result<()> {
        require_macos()?;
        self.require_user(&self.config.host.ci_user)?;
        self.install_rust()?;
        self.install_cargo_tools()?;
        self.install_android()?;
        self.install_docker_config()?;
        info!("CI user toolchain installed");
        Ok(())
    }

    fn install_rust(&self) -> Result<()> {
        let home = self.ci_home();
        let cargo_home = home.join(".cargo");
        let rustup_home = home.join(".rustup");
        let cargo_bin = cargo_home.join("bin");
        fs::create_dir_all(&cargo_bin)?;
        // The macOS guest reaches these over a share and traverses them as its
        // own account, so they have to stay readable to everyone. A directory
        // left at 0750 is not an error here and is a missing `cargo` there:
        // every job on that executor died reporting no such file.
        for directory in [&home, &cargo_home, &cargo_bin, &rustup_home] {
            make_traversable(directory)?;
        }
        let rustup_source = self
            .config
            .host
            .brew_root
            .join("opt/rustup/libexec/bin/rustup");
        if !rustup_source.is_file() {
            bail!("missing Homebrew rustup proxy: {}", rustup_source.display());
        }
        // Copying over the previous proxy left it unrunnable: the bytes change
        // but the inode does not, and macOS answers the next exec with SIGKILL
        // because the cached signature verdict describes the old content.
        super::runners::replace_file(&rustup_source, &cargo_bin.join("rustup"))
            .context("installing rustup proxy")?;
        for proxy in [
            "cargo",
            "cargo-clippy",
            "cargo-fmt",
            "clippy-driver",
            "rust-analyzer",
            "rustc",
            "rustdoc",
            "rustfmt",
        ] {
            replace_symlink(Path::new("rustup"), &cargo_bin.join(proxy))?;
        }
        let rustup = cargo_bin.join("rustup");
        let run = |args: &[&str], label: &str| -> Result<()> {
            let mut command = self.process.command(&rustup);
            command
                .env("CARGO_HOME", &cargo_home)
                .env("RUSTUP_HOME", &rustup_home)
                .args(args);
            self.process.run_command(&mut command, label)
        };
        run(
            &["set", "profile", "minimal"],
            "set Rust installation profile",
        )?;
        // `rust-src` is not optional here. Anything reaching for `-Zbuild-std`
        // needs it, and third-party tools reach for it through the channel
        // names rather than the pinned toolchain: `cargo swift` and the
        // sanitizer both asked for plain `nightly`, and the documentation
        // build asked for stable. Installing the component only on the pinned
        // nightly left each of them failing on a missing standard library.
        for toolchain in [
            &self.config.pins.stable_toolchain,
            &self.config.pins.msrv_toolchain,
            &self.config.pins.nightly_toolchain,
        ] {
            run(
                &["toolchain", "install", toolchain, "--component", "rust-src"],
                "install pinned Rust toolchain",
            )?;
        }
        run(
            &[
                "component",
                "add",
                "rustfmt",
                "--toolchain",
                &self.config.pins.nightly_toolchain,
            ],
            "install nightly formatter",
        )?;
        run(
            &[
                "toolchain",
                "install",
                "nightly",
                "--component",
                "rust-src",
                "--profile",
                "minimal",
            ],
            "install the nightly channel third-party tools resolve",
        )?;
        run(
            &[
                "target",
                "add",
                "wasm32-unknown-unknown",
                "--toolchain",
                "nightly",
            ],
            "install the WASM target on the nightly channel",
        )?;
        run(
            &["default", &self.config.pins.stable_toolchain],
            "select stable Rust",
        )?;
        run(
            &[
                "component",
                "add",
                "clippy",
                "llvm-tools-preview",
                "rustfmt",
            ],
            "install Rust components",
        )?;
        for target in [
            "aarch64-apple-ios",
            "aarch64-apple-ios-sim",
            "aarch64-linux-android",
            "wasm32-unknown-unknown",
            "x86_64-apple-ios",
            "x86_64-linux-android",
        ] {
            run(
                &[
                    "target",
                    "add",
                    target,
                    "--toolchain",
                    &self.config.pins.stable_toolchain,
                ],
                "install stable Rust target",
            )?;
        }
        run(
            &[
                "target",
                "add",
                "wasm32-unknown-unknown",
                "--toolchain",
                &self.config.pins.nightly_toolchain,
            ],
            "install nightly WASM target",
        )
    }

    fn install_cargo_tools(&self) -> Result<()> {
        let home = self.ci_home();
        let cargo = home.join(".cargo/bin/cargo");
        for (package, version) in &self.config.pins.cargo_tools {
            let mut command = self.process.command(&cargo);
            command
                .env("CARGO_HOME", home.join(".cargo"))
                .env("RUSTUP_HOME", home.join(".rustup"))
                .args(["install", "--locked", "--version", version, package]);
            self.process
                .run_command(&mut command, "install pinned Cargo tool")?;
        }
        Ok(())
    }

    fn install_android(&self) -> Result<()> {
        let root = self.config.host.host_root.join("toolchains");
        let user_home = root.join("android-user");
        let commandline = self.config.host.android_home.join("cmdline-tools/latest");
        let sdkmanager = commandline.join("bin/sdkmanager");
        let archive = root.join(format!(
            "commandlinetools-mac_arm64-{}.zip",
            self.config.pins.android_commandline_tools_version
        ));
        for path in [
            &root,
            &self.config.host.android_home,
            &user_home,
            &user_home.join("avd"),
        ] {
            fs::create_dir_all(path)?;
        }
        if !archive.is_file() {
            let url = format!(
                "https://dl.google.com/android/repository/commandlinetools-mac_arm64-{}_latest.zip",
                self.config.pins.android_commandline_tools_version
            );
            download(&url, &archive)?;
        }
        verify_sha256(&archive, &self.config.pins.android_commandline_tools_sha256)?;
        if !sdkmanager.is_file() {
            let temporary = root.join(format!("android-install.{}", std::process::id()));
            fs::create_dir(&temporary)?;
            let result = (|| {
                self.process.run(
                    "/usr/bin/ditto",
                    &["-x", "-k", path_text(&archive)?, path_text(&temporary)?],
                    "extract Android command-line tools",
                )?;
                fs::create_dir_all(&commandline)?;
                self.process.run(
                    "/usr/bin/ditto",
                    &[
                        path_text(&temporary.join("cmdline-tools"))?,
                        path_text(&commandline)?,
                    ],
                    "install Android command-line tools",
                )
            })();
            let cleanup = fs::remove_dir_all(&temporary)
                .with_context(|| format!("removing {}", temporary.display()));
            result.and(cleanup)?;
        }
        let environment = |command: &mut std::process::Command| {
            command
                .env("ANDROID_HOME", &self.config.host.android_home)
                .env("ANDROID_USER_HOME", &user_home)
                .env("ANDROID_AVD_HOME", user_home.join("avd"))
                .env("JAVA_HOME", self.config.host.java_home());
        };
        let mut licenses = self.process.command(&sdkmanager);
        environment(&mut licenses);
        licenses.arg("--licenses").stdin(Stdio::piped());
        let mut child = licenses
            .spawn()
            .context("starting Android license acceptance")?;
        child
            .stdin
            .take()
            .context("sdkmanager did not open standard input")?
            .write_all("y\n".repeat(32).as_bytes())
            .context("accepting Android licenses")?;
        if !child.wait()?.success() {
            bail!("Android SDK license acceptance failed");
        }
        let platform = self.config.pins.android_platform_version.to_string();
        let packages = [
            format!(
                "build-tools;{}",
                self.config.pins.android_build_tools_version
            ),
            "emulator".to_owned(),
            format!("ndk;{}", self.config.pins.android_ndk_version),
            "platform-tools".to_owned(),
            format!("platforms;android-{platform}"),
            format!("system-images;android-{platform};google_apis;arm64-v8a"),
        ];
        let mut install = self.process.command(&sdkmanager);
        environment(&mut install);
        install.args(&packages);
        self.process
            .run_command(&mut install, "install Android SDK packages")?;
        let avdmanager = commandline.join("bin/avdmanager");
        let current = {
            let mut command = self.process.command(&avdmanager);
            environment(&mut command);
            command.args(["list", "avd"]);
            let output = command
                .output()
                .context("listing Android virtual devices")?;
            if !output.status.success() {
                bail!("avdmanager list failed");
            }
            String::from_utf8(output.stdout).context("AVD list is not UTF-8")?
        };
        if !current
            .lines()
            .any(|line| line.trim() == format!("Name: {}", self.config.pins.android_avd))
        {
            let mut command = self.process.command(&avdmanager);
            environment(&mut command);
            command
                .args([
                    "create",
                    "avd",
                    "--force",
                    "--name",
                    &self.config.pins.android_avd,
                    "--package",
                    &format!("system-images;android-{platform};google_apis;arm64-v8a"),
                    "--device",
                    "pixel_8",
                ])
                .stdin(Stdio::piped());
            let mut child = command.spawn().context("creating Android virtual device")?;
            child
                .stdin
                .take()
                .context("avdmanager did not open standard input")?
                .write_all(b"no\n")?;
            if !child.wait()?.success() {
                bail!("Android virtual device creation failed");
            }
        }
        Ok(())
    }

    fn install_docker_config(&self) -> Result<()> {
        let directory = self.ci_home().join(".docker");
        fs::create_dir_all(&directory)?;
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "cliPluginsExtraDirs": [self.config.host.brew_root.join("lib/docker/cli-plugins")],
        }))
        .context("serializing Docker CLI configuration")?;
        write_private(
            &directory.join("config.json"),
            &[bytes, b"\n".to_vec()].concat(),
        )
    }

    fn require_user(&self, expected: &str) -> Result<()> {
        let actual = self
            .process
            .capture("/usr/bin/id", &["-un"], "current user")?;
        if actual != expected {
            bail!("run this command as {expected}, without sudo");
        }
        Ok(())
    }

    fn ci_home(&self) -> PathBuf {
        self.config
            .host
            .host_root
            .join("home")
            .join(&self.config.host.ci_user)
    }
}

fn download(url: &str, destination: &Path) -> Result<()> {
    let client = Client::builder()
        .https_only(true)
        .build()
        .context("building download client")?;
    let mut response = client
        .get(url)
        .send()
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed: {url}"))?;
    let mut file = fs::File::create(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    std::io::copy(&mut response, &mut file)
        .with_context(|| format!("writing {}", destination.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", destination.display()))
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("hashing {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != expected.to_ascii_lowercase() {
        bail!(
            "SHA-256 mismatch for {}: expected {expected}, found {actual}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn replace_symlink(source: &Path, target: &Path) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            bail!(
                "refusing to replace directory with symlink: {}",
                target.display()
            );
        }
        Ok(_) => fs::remove_file(target)
            .with_context(|| format!("removing existing proxy {}", target.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("reading {}", target.display())),
    }
    std::os::unix::fs::symlink(source, target)
        .with_context(|| format!("linking {}", target.display()))
}

#[cfg(not(unix))]
fn replace_symlink(_source: &Path, _target: &Path) -> Result<()> {
    bail!("Rust proxy installation requires Unix symlinks")
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", path.display()))?;
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
}

fn require_macos() -> Result<()> {
    if std::env::consts::OS != "macos" {
        bail!("toolchain installation supports macOS only");
    }
    Ok(())
}

/// Directories shared into the macOS guest have to stay traversable by an
/// account that is not the one that created them. The guest mounts them and
/// reads them as its own user, and a directory that lost its group and other
/// bits reports every tool inside it as missing rather than as forbidden.
#[cfg(unix)]
fn make_traversable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("reading permissions of {}", path.display()))?
        .permissions();
    let mode = permissions.mode();
    let wanted = mode | 0o055;
    if mode == wanted {
        return Ok(());
    }
    permissions.set_mode(wanted);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("making {} traversable by the guest", path.display()))
}

#[cfg(not(unix))]
fn make_traversable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_verification_rejects_modified_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive");
        fs::write(&path, b"kithara").unwrap();
        assert!(
            verify_sha256(
                &path,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            )
            .is_err()
        );
    }
}
