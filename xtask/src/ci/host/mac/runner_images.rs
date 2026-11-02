use std::{fs, path::Path, process::Stdio, thread, time::Duration};

use anyhow::{Context, Result, bail};
use tracing::info;

use super::{
    runner_guest::guest_developer_dir,
    runners::{RunnerManager, Tokens, path_text, require_macos},
};
use crate::consts;

impl RunnerManager<'_> {
    pub(super) fn smoke_android(&self) -> Result<()> {
        require_macos()?;
        self.require_ci_user()?;
        let emulator = self.config.host.android_home.join("emulator/emulator");
        let adb = self.config.host.android_home.join("platform-tools/adb");
        let log_path = self
            .config
            .host
            .host_root
            .join("logs/android-emulator-smoke.log");
        self.process.run(
            path_text(&emulator)?,
            &["-accel-check"],
            "verify Android acceleration",
        )?;
        let _ = self.process.command(&adb).args(["kill-server"]).status();
        let log = fs::File::create(&log_path)
            .with_context(|| format!("creating emulator log {}", log_path.display()))?;
        let stderr = log.try_clone().context("cloning emulator log handle")?;
        let mut child = self
            .process
            .command(&emulator)
            .env("ANDROID_HOME", &self.config.host.android_home)
            .env(
                "ANDROID_USER_HOME",
                self.config.host.host_root.join("toolchains/android-user"),
            )
            .env(
                "ANDROID_AVD_HOME",
                self.config
                    .host
                    .host_root
                    .join("toolchains/android-user/avd"),
            )
            .args([
                "-avd",
                &self.config.pins.android_avd,
                "-gpu",
                "swiftshader_indirect",
                "-no-audio",
                "-no-boot-anim",
                "-no-snapshot",
                "-no-window",
            ])
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .context("starting Android emulator")?;
        let result = (|| {
            self.process.run(
                path_text(&adb)?,
                &["wait-for-device"],
                "wait for Android emulator",
            )?;
            for _ in 0..120 {
                let boot = self
                    .process
                    .capture(
                        path_text(&adb)?,
                        &["shell", "getprop", "sys.boot_completed"],
                        "Android boot status",
                    )
                    .unwrap_or_default();
                if boot.trim() == "1" {
                    info!("Android emulator smoke passed");
                    return Ok(());
                }
                thread::sleep(Duration::from_secs(2));
            }
            bail!(
                "Android emulator did not finish booting; inspect {}",
                log_path.display()
            )
        })();
        let _ = self.process.command(&adb).args(["emu", "kill"]).status();
        let _ = child.kill();
        let _ = child.wait();
        result
    }

    /// Serves `GitLab` jobs from throwaway macOS VMs: each job gets a fresh clone
    /// of the pinned base image, and the clone is destroyed afterwards. Xcode
    /// and the Rust toolchain are mounted from the host, so the image itself
    /// stays a plain macOS install.
    pub(super) fn run_macos_runner(&self) -> Result<()> {
        require_macos()?;
        self.require_ci_user()?;
        self.verify_macos_base()?;
        let tart = path_text(&self.config.host.brew_tool("tart"))?.to_string();
        // One guest serves every job it is offered. Recreating it per job left
        // the queue waiting for a runner that did not exist yet, and the guest
        // is thrown away and rebuilt from the base image whenever it does stop,
        // so a job never inherits a half-finished predecessor.
        loop {
            self.destroy_job_vm(&tart);
            self.process.run(
                &tart,
                &["clone", self.base_vm_name()?, consts::JOB_VM_NAME],
                "clone CI macOS VM",
            )?;
            let outcome = self
                .boot_job_vm(&tart)
                .and_then(|address| self.serve_jobs(&address));
            self.destroy_job_vm(&tart);
            outcome?;
        }
    }

    fn verify_macos_base(&self) -> Result<()> {
        let bundle = &self.config.host.macos_vm_bundle;
        for part in ["config.json", "disk.img", "nvram.bin"] {
            let path = bundle.join(part);
            if !path.is_file() {
                bail!(
                    "CI macOS VM bundle is incomplete: {} is missing. Build it with \
                     `tart create --from-ipsw` for macOS build {}",
                    path.display(),
                    self.config.pins.macos_guest_build
                );
            }
        }
        Ok(())
    }

    fn base_vm_name(&self) -> Result<&str> {
        self.config
            .host
            .macos_vm_bundle
            .file_name()
            .and_then(|name| name.to_str())
            .context("macos_vm_bundle has no VM name")
    }

    fn boot_job_vm(&self, tart: &str) -> Result<String> {
        let mut command = self.process.command(tart);
        command
            // `--no-audio` is what makes the iOS Simulator usable here. With
            // the pass-through on, the guest's virtual sound device carries an
            // input channel that leads nowhere, and `AudioUnitInitialize` on
            // `AURemoteIO` inside a simulator blocks on it: CoreAudio waits
            // nine seconds, reports "Initialize: RPC timeout. Apparently
            // deadlocked" and aborts the process. Every test that starts
            // playback died that way. Without the pass-through the device is
            // output-only and the same unit initializes in two seconds. The
            // jobs never play audible sound, so nothing is lost.
            .args(["run", "--no-graphics", "--no-audio"])
            .args(self.job_vm_mounts())
            .arg(consts::JOB_VM_NAME)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.spawn().context("starting CI macOS VM")?;
        for _ in 0..consts::BOOT_ATTEMPTS {
            thread::sleep(consts::BOOT_POLL);
            if let Ok(address) =
                self.process
                    .capture(tart, &["ip", consts::JOB_VM_NAME], "CI macOS VM address")
                && !address.trim().is_empty()
            {
                return Ok(address.trim().to_string());
            }
        }
        bail!("CI macOS VM did not report an address within the boot timeout")
    }

    fn job_vm_mounts(&self) -> Vec<String> {
        let root = &self.config.host.host_root;
        let home = self.ci_home();
        let xcode = self
            .config
            .host
            .host_xcode_app()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        [
            ("Xcode.app", xcode, true),
            // `kithara-encode` links ffmpeg, and several `*-sys` crates shell
            // out to cmake and pkg-config. A stock macOS image carries none of
            // them, so share the host prefix; the guest links it back to the
            // canonical path, which the install names in these dylibs need.
            ("kithara-brew", self.config.host.brew_root.clone(), true),
            ("kithara-tools", root.join("toolchains/shared-bin"), true),
            // virtiofs cannot serve rustup's downloads, so the guest must find
            // every toolchain already installed here.
            ("kithara-rustup", home.join(".rustup"), true),
            // Cargo writes its registry and git checkouts; `concurrent = 1`
            // keeps one job from racing another over the same cache.
            ("kithara-cargo", home.join(".cargo"), false),
            ("kithara-cache", root.join("cache"), false),
        ]
        .into_iter()
        .map(|(tag, path, read_only)| {
            let suffix = if read_only { ":ro" } else { "" };
            format!("--dir={tag}:{}{suffix}", path.display())
        })
        .collect()
    }

    fn serve_jobs(&self, address: &str) -> Result<()> {
        let shared = consts::GUEST_SHARE;
        let brew = self.config.host.brew_root.join("bin");
        let brew = brew.display();
        let path = format!("$HOME/.cargo/bin:{brew}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
        let tokens = Tokens::load(
            &self.ci_home().join(".config/kithara-ci"),
            self.config.host.job_concurrency,
        )?;
        // Move the share off the auto-mounted path before anything reads it:
        // its name contains spaces, which GNU make cannot express. Nothing may
        // be running from the mount at this point, so this is the only moment
        // the unmount can succeed.
        self.guest_shell(
            address,
            &format!(
                "set -e; \
                 if [ ! -d {shared}/Xcode.app ]; then \
                 sudo -n diskutil unmount force '{}' >/dev/null; \
                 sudo -n install -d -m 0755 {shared}; \
                 sudo -n mount_virtiofs com.apple.virtio-fs.automount {shared}; \
                 fi",
                self.config.host.macos_guest_shared_root.display(),
            ),
            "remount guest share without spaces",
            None,
        )?;
        self.guest_shell(
            address,
            &format!(
                "export PATH={path}; \
                 {shared}/kithara-tools/kithara-ci ci host mac --config \
                 {shared}/kithara-tools/mac-host.toml --pins \
                 {shared}/kithara-tools/pins.toml guest-prepare"
            ),
            "prepare CI macOS guest",
            None,
        )?;
        // One guest serves a run of jobs rather than one, so the second job of
        // a pipeline does not wait for a new one to boot — but it serves a
        // bounded run, because the space each job leaves behind comes back
        // only when the guest is destroyed.
        self.guest_shell(
            address,
            &format!(
                "read -r RUNNER_TOKEN; \
                 export PATH={path}; \
                 export DEVELOPER_DIR={}; \
                 export KITHARA_CI_CACHE_ROOT={shared}/kithara-cache; \
                 exec {shared}/kithara-tools/gitlab-runner run-single --url {} \
                 --token \"$RUNNER_TOKEN\" --executor shell --shell bash \
                 --max-builds {} --wait-timeout {}",
                guest_developer_dir(),
                self.config.host.gitlab_origin(),
                consts::MAX_BUILDS,
                consts::WAIT_SECONDS,
            ),
            "serve GitLab jobs",
            tokens.macos.first().map(String::as_str),
        )
    }

    /// The runner token reaches the guest over stdin so it never lands in
    /// `argv`, where any local process could read it.
    fn guest_shell(
        &self,
        address: &str,
        script: &str,
        label: &str,
        stdin: Option<&str>,
    ) -> Result<()> {
        let mut command = self.process.command("/usr/bin/ssh");
        command
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                &format!("{}@{address}", self.config.host.macos_guest_user),
                script,
            ])
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command
            .spawn()
            .with_context(|| format!("running guest command: {label}"))?;
        if let Some(secret) = stdin
            && let Some(pipe) = child.stdin.as_mut()
        {
            use std::io::Write;
            writeln!(pipe, "{secret}").context("passing the runner token to the guest")?;
        }
        let status = child
            .wait()
            .with_context(|| format!("waiting for guest command: {label}"))?;
        if !status.success() {
            bail!(
                "{label} failed with exit code {}",
                status.code().unwrap_or(-1)
            );
        }
        Ok(())
    }

    fn destroy_job_vm(&self, tart: &str) {
        self.process
            .best_effort(tart, &["stop", consts::JOB_VM_NAME], "stop CI macOS VM");
        self.process
            .best_effort(tart, &["delete", consts::JOB_VM_NAME], "delete CI macOS VM");
    }
}
