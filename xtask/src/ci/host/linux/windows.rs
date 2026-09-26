use std::{collections::BTreeMap, fs, io::Read, path::Path, thread, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::info;

use super::{
    profile::{LinuxHost, WindowsGuest},
    registration,
};
use crate::{
    ci::{config::CiPins, process::Process},
    consts,
};

/// What the guest needs to register itself, written to the volume it reads its
/// answers from. The token is good for an hour and for one registration.
#[derive(Serialize)]
struct Enrolment<'a> {
    labels: String,
    name: &'a str,
    token: &'a str,
    url: String,
}

/// Everything the guest's first sign-in needs to know, written beside the
/// answer file so the script carries no versions of its own.
#[derive(Serialize)]
struct GuestSettings<'a> {
    build_tools_url: &'a str,
    /// Empty: Microsoft replaces this bootstrapper in place, so the guest
    /// verifies its signature instead. See .config/windows/provision.ps1.
    build_tools_sha256: &'a str,
    cargo_tools: BTreeMap<&'a str, &'a str>,
    cmake_sha256: &'a str,
    cmake_url: String,
    git_sha256: &'a str,
    git_url: String,
    runner_sha256: &'a str,
    runner_url: String,
    rustup_sha256: &'a str,
    rustup_url: String,
    stable_toolchain: &'a str,
}

/// Install the Windows guest that serves the Windows lane.
///
/// Windows Setup reads its answers from any attached volume, so the guest is
/// built by handing it two disks: the installation media, and a small one
/// carrying the answer file, the provisioning script, and the pinned versions
/// that script installs. Nothing is typed into a console.
pub(super) fn install(
    process: &Process,
    host: &LinuxHost,
    pins: &CiPins,
    root: &Path,
) -> Result<()> {
    let guest = host
        .windows
        .as_ref()
        .context("this machine's profile defines no Windows guest")?;
    process.require_tools(&["virsh", "virt-install", "xorriso"])?;

    // libvirt ships its network defined but not started, and a guest attached
    // to a network that is down installs with no way to reach anything. Its
    // state is read back rather than inferred from the start succeeding: a
    // network that was already up reports an error that means nothing is wrong.
    let _ = process.run(
        "virsh",
        &["net-start", &guest.network],
        "start the guest network",
    );
    let state = process.capture(
        "virsh",
        &["net-info", &guest.network],
        "read the guest network",
    )?;
    if !state
        .lines()
        .any(|line| line.starts_with("Active:") && line.contains("yes"))
    {
        bail!(
            "the guest network {} is not running; libvirt reported:\n{state}",
            guest.network
        );
    }

    let media = host.cache_root.join("iso/windows-eval.iso");
    verify_media(
        &media,
        &pins.windows_eval_iso_sha256,
        &pins.windows_eval_iso_url,
    )?;

    let answers = build_answer_media(process, host, pins, root)?;

    let mut command = process.command("virt-install");
    command.args([
        "--name",
        &guest.name,
        "--osinfo",
        "win11",
        "--boot",
        "uefi",
        "--tpm",
        "backend.type=emulator,backend.version=2.0,model=tpm-crb",
        "--vcpus",
        &guest.vcpus.to_string(),
        "--memory",
        &guest.memory_mib.to_string(),
        "--disk",
        &format!("size={},format=qcow2", guest.disk_gib),
        // The installation media is the install method, not just another disk.
        "--cdrom",
        path_text(&media)?,
        "--disk",
        &format!("{},device=cdrom", path_text(&answers)?),
        "--network",
        &format!("network={}", guest.network),
        // Windows Setup draws its progress on a screen and does nothing
        // without one, so the guest gets a display. It listens on the loopback
        // address only: the machine's own operator can watch an install, and
        // nobody else can reach it.
        "--graphics",
        "vnc,listen=127.0.0.1",
        "--noautoconsole",
    ]);
    process.run_command(&mut command, "create the Windows guest")?;

    press_a_key(process, &guest.name)?;

    // Windows installs in phases with a power cycle between them, and
    // virt-install leaves the guest set to stay down after the first one so a
    // caller can inspect it. A CI machine has nobody to inspect it: the guest
    // should come back on its own until it is a runner.
    process.require_tools(&["virt-xml"])?;
    process.run(
        "virt-xml",
        &[&guest.name, "--edit", "--events", "on_poweroff=restart"],
        "keep the guest through its install phases",
    )?;

    resume_first_phase(process, &guest.name)?;

    info!(
        guest = guest.name,
        "Windows guest created; the unattended install runs on its own from here"
    );
    Ok(())
}

/// Start the guest again after the one power-off the edit above cannot cover.
///
/// libvirt applies an edited event to a domain's next start, and the power-off
/// that ends the first install phase comes before any start it could apply to —
/// so the guest goes down still carrying the setting it was created with, and
/// stays there. Waiting for that one power-off and starting the guest again is
/// enough: every later phase is covered by the setting, which is now live.
fn resume_first_phase(process: &Process, guest: &str) -> Result<()> {
    for _ in 0..consts::GUEST_FIRST_PHASE_ATTEMPTS {
        thread::sleep(Duration::from_secs(30));
        let state = process.capture("virsh", &["domstate", guest], "read the guest state")?;
        if state.trim() == "shut off" {
            return process.run("virsh", &["start", guest], "resume the install");
        }
    }
    // Setup can also get through its phases on reboots alone, which libvirt
    // already restarts. Nothing to resume then.
    info!(guest, "the guest stayed up through its first phase");
    Ok(())
}

/// Make the installed guest a runner for this repository.
///
/// The guest reads its answers from a small disc rather than from a network
/// service the host would have to run, so its credentials arrive the same way:
/// the disc it already has is replaced with one carrying an enrolment, and the
/// guest picks it up at its next sign-in. The token never passes through a
/// command line.
///
/// The guest is asked to report back rather than assumed to have: a machine
/// that booted and did nothing looks exactly like one that enrolled, from the
/// host's side.
pub(super) fn enrol(process: &Process, host: &LinuxHost) -> Result<()> {
    let guest = host
        .windows
        .as_ref()
        .context("this machine's profile defines no Windows guest")?;
    process.require_tools(&["virsh", "xorriso"])?;

    let media = build_enrolment_media(
        process,
        host,
        guest,
        &registration::enrolment_token(host, guest)?,
    )?;
    process.run(
        "virsh",
        &[
            "change-media",
            &guest.name,
            path_text(&host.cache_root.join("windows-answers.iso"))?,
            path_text(&media)?,
            "--update",
            "--config",
            "--live",
        ],
        "hand the guest its enrolment",
    )?;
    // A guest that is already up has read the old disc; one that is down has
    // read nothing. Either way it signs in from cold and finds the new one.
    let _ = process.run("virsh", &["destroy", &guest.name], "put the guest down");
    process.run("virsh", &["start", &guest.name], "start the guest")?;

    for _ in 0..consts::GUEST_ENROLMENT_ATTEMPTS {
        thread::sleep(Duration::from_secs(10));
        if registration::is_online(host, guest)? {
            info!(guest = guest.name, "the guest is registered and waiting");
            return Ok(());
        }
    }
    bail!(
        "the guest {} never reported for work; watch it over VNC",
        guest.name
    )
}

/// Build the disc the guest reads its enrolment from. It replaces the one that
/// carried the answer file, so the drive letter the guest reads does not move.
fn build_enrolment_media(
    process: &Process,
    host: &LinuxHost,
    guest: &WindowsGuest,
    token: &str,
) -> Result<std::path::PathBuf> {
    let staging = host.cache_root.join("windows-enrolment");
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| format!("clearing {}", staging.display()))?;
    }
    fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;

    let enrolment = Enrolment {
        labels: guest.labels.join(","),
        name: &guest.name,
        token,
        url: format!(
            "https://github.com/{}",
            host.credential(&guest.repository)?.name
        ),
    };
    fs::write(
        staging.join("enrolment.json"),
        serde_json::to_vec_pretty(&enrolment).context("serialising the enrolment")?,
    )
    .context("writing the enrolment")?;

    let media = host.cache_root.join("windows-enrolment.iso");
    let mut command = process.command("xorriso");
    command.args([
        "-as",
        "mkisofs",
        "-quiet",
        "-J",
        "-rock",
        "-volid",
        "ANSWERS",
        "-output",
        path_text(&media)?,
        path_text(&staging)?,
    ]);
    process.run_command(&mut command, "build the enrolment media")?;
    fs::remove_dir_all(&staging).with_context(|| format!("clearing {}", staging.display()))?;
    Ok(media)
}

/// Get past the prompt that stands between a Windows disc and an unattended
/// install.
///
/// Windows installation media asks for a keypress before it will boot, and
/// answers nothing if none arrives: the firmware moves on to a disk with no
/// operating system on it and stops. The prompt appears a few seconds after
/// the guest starts and lasts a few more, so the key is sent across that
/// window rather than at a moment that would have to be guessed exactly.
///
/// Sending stops the moment the disk starts filling. Past that point Setup is
/// running and reads the same keys as its own: a stray Enter reaches the
/// Cancel button and asks whether to abandon the install.
fn press_a_key(process: &Process, guest: &str) -> Result<()> {
    let idle = written_bytes(process, guest)?;
    for _ in 0..consts::GUEST_PROMPT_ATTEMPTS {
        thread::sleep(Duration::from_secs(1));
        if written_bytes(process, guest)? > idle {
            info!(guest, "the guest is installing");
            return Ok(());
        }
        let _ = process.run(
            "virsh",
            &["send-key", guest, "KEY_ENTER"],
            "answer the boot prompt",
        );
    }
    bail!("the guest never started writing to its disk; watch it over VNC")
}

/// How much of the guest's own disk is filled. libvirt reports this per
/// device; the first one is the disk the guest installs onto.
fn written_bytes(process: &Process, guest: &str) -> Result<u64> {
    let report = process.capture(
        "virsh",
        &["domstats", guest, "--block"],
        "read the guest's disk usage",
    )?;
    report
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("block.0.allocation=")?
                .parse()
                .ok()
        })
        .context("libvirt reported no allocation for the guest's first disk")
}

/// Build the small disk Windows Setup reads its answers from.
fn build_answer_media(
    process: &Process,
    host: &LinuxHost,
    pins: &CiPins,
    root: &Path,
) -> Result<std::path::PathBuf> {
    let staging = host.cache_root.join("windows-answers");
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| format!("clearing {}", staging.display()))?;
    }
    fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;

    for source in [consts::GUEST_ANSWER_FILE, consts::GUEST_PROVISION_SCRIPT] {
        let name = Path::new(source)
            .file_name()
            .context("a tracked file with no name")?;
        fs::copy(root.join(source), staging.join(name))
            .with_context(|| format!("copying {source}"))?;
    }

    let settings = GuestSettings {
        build_tools_url: consts::GUEST_BUILD_TOOLS_URL,
        build_tools_sha256: "",
        cargo_tools: consts::GUEST_CARGO_TOOLS
            .iter()
            .map(|tool| Ok((*tool, pins.cargo_tool_version(tool)?)))
            .collect::<Result<_>>()?,
        cmake_sha256: &pins.cmake_windows_amd64_sha256,
        cmake_url: format!(
            "https://github.com/Kitware/CMake/releases/download/v{version}/cmake-{version}-windows-x86_64.zip",
            version = pins.cmake_version,
        ),
        git_sha256: &pins.git_windows_sha256,
        git_url: pins.git_windows_url.clone(),
        runner_sha256: &pins.actions_runner_windows_sha256,
        runner_url: format!(
            "https://github.com/actions/runner/releases/download/v{version}/actions-runner-win-x64-{version}.zip",
            version = pins.actions_runner_version,
        ),
        rustup_sha256: &pins.rustup_windows_sha256,
        rustup_url: format!(
            "https://static.rust-lang.org/rustup/archive/{}/x86_64-pc-windows-msvc/rustup-init.exe",
            pins.rustup_version,
        ),
        stable_toolchain: &pins.stable_toolchain,
    };
    fs::write(
        staging.join("guest.json"),
        serde_json::to_vec_pretty(&settings).context("serialising the guest settings")?,
    )
    .context("writing the guest settings")?;

    let media = host.cache_root.join("windows-answers.iso");
    let mut command = process.command("xorriso");
    command.args([
        "-as",
        "mkisofs",
        "-quiet",
        "-J",
        "-rock",
        "-volid",
        "ANSWERS",
        "-output",
        path_text(&media)?,
        path_text(&staging)?,
    ]);
    process.run_command(&mut command, "build the answer media")?;
    Ok(media)
}

/// Refuse to install from media the pins do not vouch for. A guest built from
/// a substituted image would look identical from the outside.
fn verify_media(path: &Path, expected: &str, source: &str) -> Result<()> {
    if !path.is_file() {
        bail!(
            "the Windows installation media is missing from {}; download it from {source}",
            path.display()
        );
    }
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
    if actual != expected {
        bail!(
            "the Windows installation media at {} hashes to {actual}, not the pinned {expected}",
            path.display()
        );
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::ci::config::fixture;

    /// Both names are repository paths resolved at install time, on a host, in
    /// a step that runs months apart from any edit to this file. A rename that
    /// missed one of them would surface there and nowhere earlier.
    #[test]
    fn the_guest_sources_name_files_this_repository_tracks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits inside the workspace");
        for source in [consts::GUEST_ANSWER_FILE, consts::GUEST_PROVISION_SCRIPT] {
            assert!(root.join(source).is_file(), "{source}");
        }
    }

    #[test]
    fn the_guest_is_told_versions_rather_than_left_to_choose() {
        let pins = &fixture().pins;
        let settings = GuestSettings {
            build_tools_url: consts::GUEST_BUILD_TOOLS_URL,
            build_tools_sha256: "",
            cargo_tools: consts::GUEST_CARGO_TOOLS
                .iter()
                .map(|tool| (*tool, pins.cargo_tool_version(tool).unwrap()))
                .collect(),
            cmake_sha256: &pins.cmake_windows_amd64_sha256,
            cmake_url: String::new(),
            git_sha256: &pins.git_windows_sha256,
            git_url: String::new(),
            runner_sha256: &pins.actions_runner_windows_sha256,
            runner_url: String::new(),
            rustup_sha256: &pins.rustup_windows_sha256,
            rustup_url: String::new(),
            stable_toolchain: &pins.stable_toolchain,
        };
        let json = serde_json::to_string(&settings).unwrap();

        assert!(json.contains(pins.stable_toolchain.as_str()), "{json}");
        assert!(
            json.contains(pins.cargo_tool_version("cargo-nextest").unwrap()),
            "{json}"
        );
    }
}
