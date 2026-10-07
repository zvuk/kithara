use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
    ffmpeg_sha256: &'a str,
    ffmpeg_url: &'a str,
    llvm_sha256: &'a str,
    llvm_url: String,
    monkeys_audio_source_url: &'a str,
    monkeys_audio_source_sha256: &'a str,
    git_sha256: &'a str,
    git_url: String,
    runner_sha256: &'a str,
    runner_url: String,
    rustup_sha256: &'a str,
    rustup_url: String,
    stable_toolchain: &'a str,
}

/// How the guest last said provisioning itself went.
#[derive(Debug, PartialEq, Eq)]
enum Provisioning {
    /// Nothing final yet.
    Running,
    Done,
    Failed(String),
}

/// One device of a guest, as `virsh domblklist --details` lists it.
struct Drive {
    kind: String,
    device: String,
    target: String,
    source: PathBuf,
}

/// Install the Windows guest that serves the Windows lane, and stay until it
/// is a registered runner.
///
/// Windows Setup reads its answers from any attached volume, so the guest is
/// built by handing it two disks: the installation media, and a small one
/// carrying the answer file, the provisioning script, and the pinned versions
/// that script installs. Nothing is typed into a console. A guest this
/// replaces is removed first, so the same command builds the first guest and
/// every one after it.
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
    let media = prepare(process, host, guest, pins)?;
    rebuild(process, host, guest, pins, root, &media)
}

/// Check what a guest is built from before anything is taken down: the
/// network it attaches to, and the installation media, which is returned.
fn prepare(
    process: &Process,
    host: &LinuxHost,
    guest: &WindowsGuest,
    pins: &CiPins,
) -> Result<PathBuf> {
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
    Ok(media)
}

/// Replace whatever guest is there with a new one, and stay until it is a
/// registered runner.
fn rebuild(
    process: &Process,
    host: &LinuxHost,
    guest: &WindowsGuest,
    pins: &CiPins,
    root: &Path,
    media: &Path,
) -> Result<()> {
    remove(process, host, guest)?;
    let answers = build_answer_media(process, host, pins, root)?;

    let mut command = process.command("virt-install");
    command.args(creation_args(host, guest, media, &answers)?);
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
    await_provisioning(host, guest)?;
    enrol(process, host)
}

/// What `virt-install` is told the guest is.
fn creation_args(
    host: &LinuxHost,
    guest: &WindowsGuest,
    media: &Path,
    answers: &Path,
) -> Result<Vec<String>> {
    Ok([
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
        path_text(media)?,
        "--disk",
        &format!("{},device=cdrom", path_text(answers)?),
        "--network",
        &format!("network={}", guest.network),
        // Windows Setup draws its progress on a screen and does nothing
        // without one, so the guest gets a display. It listens on the loopback
        // address only: the machine's own operator can watch an install, and
        // nobody else can reach it.
        "--graphics",
        "vnc,listen=127.0.0.1",
        // What the guest writes to its first serial port lands in this file:
        // how provisioning went, and later when its licence runs out. Nothing
        // else of a guest that is not yet a runner reaches the host.
        "--serial",
        &format!("file,path={}", path_text(&console_log(host))?),
        // The suite plays through the machine's default audio output, and
        // Windows has one only for a sound card it can see. The host plays
        // what reaches the card into nothing, at the rate a card would.
        "--sound",
        "model=ich9",
        "--noautoconsole",
    ]
    .map(str::to_owned)
    .into())
}

/// Build the guest again when its evaluation licence is about to run out.
///
/// An expired evaluation shuts itself down every hour, which ends a test run
/// mid-suite, and the licence can be rearmed only so often. A new guest starts
/// a new evaluation, so a daily timer runs this and the guest is rebuilt while
/// there is still a week left, never under a job.
pub(super) fn renew(process: &Process, host: &LinuxHost, pins: &CiPins, root: &Path) -> Result<()> {
    let guest = host
        .windows
        .as_ref()
        .context("this machine's profile defines no Windows guest")?;
    let days = licence_days_left(host)?;
    if !needs_renewal(days, false) {
        info!(guest = guest.name, licence_days_left = ?days, "the guest is kept");
        return Ok(());
    }
    // The media is checked before the runner is asked, not after: hashing it
    // takes a minute, and a job could land on the guest in that minute.
    let media = prepare(process, host, guest, pins)?;
    let busy = registration::is_busy(host, guest)?;
    if !needs_renewal(days, busy) {
        info!(guest = guest.name, licence_days_left = ?days, busy, "the guest is kept");
        return Ok(());
    }
    info!(
        guest = guest.name,
        licence_days_left = ?days,
        "the guest's licence is running out; building it again"
    );
    rebuild(process, host, guest, pins, root, &media)
}

/// Report the guest's state and how long its licence has left.
pub(super) fn health(process: &Process, host: &LinuxHost) -> Result<()> {
    let Some(guest) = &host.windows else {
        return Ok(());
    };
    process.require_tools(&["virsh"])?;
    let state = if defined(process, &guest.name)? {
        process.capture("virsh", &["domstate", &guest.name], "read the guest state")?
    } else {
        "undefined".to_owned()
    };
    info!(
        guest = guest.name,
        state,
        licence_days_left = ?licence_days_left(host)?,
        "Windows guest"
    );
    Ok(())
}

/// Whether the guest should be built again now. One that has not said when
/// its licence runs out is left alone: it is still booting, or broken in a way
/// a nightly rebuild would only hide.
fn needs_renewal(days_left: Option<u64>, busy: bool) -> bool {
    days_left.is_some_and(|days| days < consts::GUEST_RENEWAL_DAYS) && !busy
}

/// Whole days left on the guest's licence, as it last reported it.
fn licence_days_left(host: &LinuxHost) -> Result<Option<u64>> {
    let path = console_log(host);
    if !path.exists() {
        return Ok(None);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("this machine's clock is before 1970")?
        .as_secs();
    Ok(licence_expiry(&read_console(&path)?).map(|expiry| days_left(expiry, now)))
}

fn days_left(expiry: u64, now: u64) -> u64 {
    expiry.saturating_sub(now) / (24 * 60 * 60)
}

/// When the guest's licence runs out, in seconds since the epoch. The guest
/// reports it at every sign-in, so the last report is the current one.
fn licence_expiry(console: &str) -> Option<u64> {
    guest_lines(console)
        .rev()
        .find_map(|line| line.strip_prefix("licence-expires ")?.trim().parse().ok())
}

fn provisioning(console: &str) -> Provisioning {
    guest_lines(console)
        .rev()
        .find_map(|line| {
            if line == "done" {
                return Some(Provisioning::Done);
            }
            line.strip_prefix("failed:")
                .map(|error| Provisioning::Failed(error.trim().to_owned()))
        })
        .unwrap_or(Provisioning::Running)
}

/// What the guest wrote for the host, oldest first. The firmware writes to
/// the same port before Windows starts, so only the guest's own lines count,
/// wherever on a line the firmware left them.
fn guest_lines(console: &str) -> impl DoubleEndedIterator<Item = &str> {
    console.lines().filter_map(|line| {
        line.split_once(consts::GUEST_CONSOLE_PREFIX)
            .map(|(_, said)| said.trim())
    })
}

/// The last of what the guest said, for a failure to carry.
fn tail(console: &str) -> String {
    let lines: Vec<&str> = guest_lines(console).collect();
    lines[lines.len().saturating_sub(consts::GUEST_CONSOLE_TAIL)..].join("\n")
}

fn console_log(host: &LinuxHost) -> PathBuf {
    host.cache_root.join("windows-console.log")
}

/// The console holds the firmware's output as well as the guest's, which is
/// not promised to be text.
fn read_console(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Wait for the guest to say how provisioning itself ended.
///
/// From outside, a guest stuck in an installer and one an hour into compiling
/// both just look running; its console is the only place the difference
/// shows. The guest names each step as it starts it, and ends with `done` or
/// with what failed.
fn await_provisioning(host: &LinuxHost, guest: &WindowsGuest) -> Result<()> {
    let path = console_log(host);
    let mut reported = String::new();
    for _ in 0..consts::GUEST_PROVISION_ATTEMPTS {
        thread::sleep(Duration::from_secs(30));
        let console = read_console(&path)?;
        if let Some(step) = guest_lines(&console)
            .filter_map(|line| line.strip_prefix("step "))
            .next_back()
            && step != reported
        {
            info!(guest = guest.name, step, "provisioning");
            step.clone_into(&mut reported);
        }
        match provisioning(&console) {
            Provisioning::Done => return Ok(()),
            Provisioning::Failed(error) => bail!(
                "the guest {} failed to provision itself: {error}\nits console ended:\n{}",
                guest.name,
                tail(&console)
            ),
            Provisioning::Running => {}
        }
    }
    bail!(
        "the guest {} never finished provisioning itself; its console ended:\n{}",
        guest.name,
        tail(&read_console(&path)?)
    )
}

/// Take down the guest a previous install left, and everything that was its
/// alone: the domain with its firmware variables and TPM state, the disk it
/// installed onto, and what it wrote to its console, which would otherwise
/// answer for the guest that replaces it. The installation media is the
/// host's and stays.
fn remove(process: &Process, host: &LinuxHost, guest: &WindowsGuest) -> Result<()> {
    if defined(process, &guest.name)? {
        let listing = process.capture(
            "virsh",
            &["domblklist", &guest.name, "--details"],
            "list the guest's drives",
        )?;
        stop(process, &guest.name)?;
        process.run(
            "virsh",
            &["undefine", &guest.name, "--nvram", "--tpm"],
            "remove the old guest",
        )?;
        for drive in drives(&listing)
            .into_iter()
            .filter(|drive| drive.kind == "file" && drive.device == "disk")
        {
            fs::remove_file(&drive.source)
                .with_context(|| format!("removing {}", drive.source.display()))?;
            info!(disk = %drive.source.display(), "the old guest's disk is removed");
        }
    }
    let console = console_log(host);
    if console.exists() {
        fs::remove_file(&console).with_context(|| format!("removing {}", console.display()))?;
    }
    Ok(())
}

/// Whether libvirt knows a guest of this name, running or not.
fn defined(process: &Process, guest: &str) -> Result<bool> {
    let names = process.capture("virsh", &["list", "--all", "--name"], "list the guests")?;
    Ok(names.lines().any(|name| name.trim() == guest))
}

/// Power the guest off at once. One that is already off is what was asked for.
fn stop(process: &Process, guest: &str) -> Result<()> {
    process.ensure("virsh", &["destroy", guest], "stop the guest", |output| {
        String::from_utf8_lossy(&output.stderr).contains("domain is not running")
    })
}

/// The devices a listing names. The source is the rest of the line, spaces
/// and all. A drive with nothing in it is listed with `-` for a source, and
/// is no file of anyone's.
fn drives(listing: &str) -> Vec<Drive> {
    listing
        .lines()
        .filter_map(|line| {
            let (kind, rest) = line.trim().split_once(char::is_whitespace)?;
            let (device, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            let (target, source) = rest.trim_start().split_once(char::is_whitespace)?;
            let drive = Drive {
                kind: kind.to_owned(),
                device: device.to_owned(),
                target: target.to_owned(),
                source: PathBuf::from(source.trim()),
            };
            drive.source.is_absolute().then_some(drive)
        })
        .collect()
}

/// The drive holding whichever disc the guest last read its answers from:
/// the one install handed it, or a previous enrolment.
fn answer_drive(listing: &str, host: &LinuxHost) -> Option<String> {
    let discs = [
        host.cache_root.join("windows-answers.iso"),
        host.cache_root.join("windows-enrolment.iso"),
    ];
    drives(listing)
        .into_iter()
        .find(|drive| drive.device == "cdrom" && discs.contains(&drive.source))
        .map(|drive| drive.target)
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

    let listing = process.capture(
        "virsh",
        &["domblklist", &guest.name, "--details"],
        "list the guest's drives",
    )?;
    let drive = answer_drive(&listing, host).with_context(|| {
        format!(
            "the guest {} holds neither its answers nor an enrolment",
            guest.name
        )
    })?;
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
            &drive,
            path_text(&media)?,
            "--update",
            "--config",
            "--live",
        ],
        "hand the guest its enrolment",
    )?;
    // A guest that is already up has read the old disc; one that is down has
    // read nothing. Either way it signs in from cold and finds the new one.
    stop(process, &guest.name)?;
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
) -> Result<PathBuf> {
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
) -> Result<PathBuf> {
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

    fs::write(
        staging.join("guest.json"),
        serde_json::to_vec_pretty(&guest_settings(pins)?)
            .context("serialising the guest settings")?,
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

fn guest_settings(pins: &CiPins) -> Result<GuestSettings<'_>> {
    Ok(GuestSettings {
        build_tools_url: consts::GUEST_BUILD_TOOLS_URL,
        build_tools_sha256: "",
        cargo_tools: consts::GUEST_CARGO_TOOLS
            .iter()
            .map(|tool| Ok((*tool, pins.cargo_tool_version(tool)?)))
            .collect::<Result<_>>()?,
        monkeys_audio_source_url: &pins.monkeys_audio_source_url,
        monkeys_audio_source_sha256: &pins.monkeys_audio_source_sha256,
        cmake_sha256: &pins.cmake_windows_amd64_sha256,
        cmake_url: format!(
            "https://github.com/Kitware/CMake/releases/download/v{version}/cmake-{version}-windows-x86_64.zip",
            version = pins.cmake_version,
        ),
        ffmpeg_sha256: &pins.ffmpeg_windows_sha256,
        ffmpeg_url: &pins.ffmpeg_windows_url,
        llvm_sha256: &pins.llvm_windows_amd64_sha256,
        llvm_url: format!(
            "https://github.com/llvm/llvm-project/releases/download/llvmorg-{version}/LLVM-{version}-win64.msi",
            version = pins.llvm_version,
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
    })
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
    use crate::ci::{config::fixture, process::Recording};

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
        let json = serde_json::to_string(&guest_settings(pins).unwrap()).unwrap();

        for pinned in [
            pins.stable_toolchain.as_str(),
            pins.cargo_tool_version("cargo-nextest").unwrap(),
            pins.ffmpeg_windows_sha256.as_str(),
            pins.llvm_version.as_str(),
        ] {
            assert!(json.contains(pinned), "{pinned}: {json}");
        }
    }

    /// The firmware writes to the same serial port before Windows does, so a
    /// console holds escape sequences and boot chatter around what the guest
    /// said, sometimes on the same line.
    fn console(lines: &[&str]) -> String {
        let mut text = String::from("\u{1b}[2J\u{1b}[01;01HBdsDxe: loading Boot0001\r\n");
        for line in lines {
            text.push_str("\u{1b}[0m");
            text.push_str(&format!("{} {line}\r\n", consts::GUEST_CONSOLE_PREFIX));
        }
        text
    }

    #[test]
    fn provisioning_ends_with_the_last_word_the_guest_wrote() {
        assert_eq!(provisioning(""), Provisioning::Running);
        assert_eq!(
            provisioning(&console(&["step Installing CMake"])),
            Provisioning::Running
        );
        assert_eq!(
            provisioning(&console(&["step Installing CMake", "done"])),
            Provisioning::Done
        );
        assert_eq!(
            provisioning(&console(&[
                "step Installing Monkey's Audio",
                "failed: Monkey's Audio build failed",
            ])),
            Provisioning::Failed("Monkey's Audio build failed".to_owned())
        );
    }

    /// The guest reports at every sign-in, and a later report replaces an
    /// earlier one: a rearm moves the date forward.
    #[test]
    fn the_licence_ends_when_the_guest_last_said_it_does() {
        assert_eq!(licence_expiry(&console(&["done"])), None);
        assert_eq!(
            licence_expiry(&console(&[
                "licence-expires 1800000000",
                "licence-expires 1807776000",
            ])),
            Some(1_807_776_000)
        );

        let now = 1_800_000_000;
        assert_eq!(days_left(now + 7 * 86_400, now), 7);
        assert_eq!(days_left(now + 7 * 86_400 - 1, now), 6);
        assert_eq!(days_left(now - 86_400, now), 0, "an expired licence");
    }

    /// Building the guest again takes it away for two hours, so it happens
    /// only for a licence that is about to run out, and never under a job. A
    /// guest that has not said when its licence ends is left alone: it is
    /// booting, or broken in a way a rebuild every night would only hide.
    #[test]
    fn a_guest_is_rebuilt_only_when_its_licence_runs_short_and_it_runs_no_job() {
        let short = consts::GUEST_RENEWAL_DAYS - 1;
        let enough = consts::GUEST_RENEWAL_DAYS;
        assert!(needs_renewal(Some(short), false));
        assert!(needs_renewal(Some(0), false));
        assert!(!needs_renewal(Some(short), true));
        assert!(!needs_renewal(Some(enough), false));
        assert!(!needs_renewal(None, false));
    }

    /// What the host waits for has to be something the guest writes. The
    /// two halves live in different languages and nothing else ties them:
    /// a renamed marker would surface as a two-hour timeout on a guest that
    /// finished long before.
    #[test]
    fn the_guest_writes_what_the_host_reads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits inside the workspace");
        let script = fs::read_to_string(root.join(consts::GUEST_PROVISION_SCRIPT)).unwrap();
        for marker in ["step ", "done", "failed: ", "licence-expires "] {
            let line = format!("{} {marker}", consts::GUEST_CONSOLE_PREFIX);
            assert!(script.contains(&line), "the guest never writes `{line}`");
        }
    }

    /// A guest is rebuilt under the same name onto a disk of the same name,
    /// so whatever the last one left is removed first: the domain with its
    /// firmware variables and TPM, the disk it installed onto, and its
    /// console, which would otherwise answer for the new guest. The
    /// installation media is the host's, and costs a download to replace.
    #[test]
    fn rebuilding_a_guest_takes_what_was_its_own_and_keeps_the_media() {
        let directory = tempfile::tempdir().unwrap();
        let mut host = super::super::profile::tests::host_fixture();
        host.cache_root = directory.path().to_path_buf();
        let guest = host.windows.clone().unwrap();
        let disk = directory.path().join("kithara-ci-windows.qcow2");
        let media = directory.path().join("iso/windows-eval.iso");
        fs::create_dir_all(media.parent().unwrap()).unwrap();
        for file in [&disk, &media, &console_log(&host)] {
            fs::write(file, "").unwrap();
        }
        let listing = format!(
            " Type   Device   Target   Source\n\
             ------------------------------------------------\n \
             file   disk     sda      {}\n \
             file   cdrom    sdb      {}\n \
             file   cdrom    sdc      -\n",
            disk.display(),
            media.display(),
        );
        let process = Process::recording(
            directory.path(),
            Recording::default()
                .with_reply(&["virsh", "list"], &format!("other\n{}\n", guest.name))
                .with_reply(&["virsh", "domblklist"], &listing),
        );

        remove(&process, &host, &guest).unwrap();

        assert!(!disk.exists(), "the old system disk is left behind");
        assert!(
            !console_log(&host).exists(),
            "the old console is left behind"
        );
        assert!(media.exists(), "the installation media is gone");
        let steps = process.recorded().unwrap();
        assert!(
            steps.steps().iter().any(|step| {
                step.program == "virsh"
                    && ["undefine", &guest.name, "--nvram", "--tpm"]
                        .iter()
                        .all(|part| step.args.iter().any(|arg| arg == part))
            }),
            "{:?}",
            steps.steps()
        );
    }

    /// The enrolment replaces the disc the guest reads its answers from,
    /// which after a first enrolment is the previous enrolment: registering
    /// the guest again has to find it there too.
    #[test]
    fn an_enrolment_replaces_whichever_answer_disc_the_guest_holds() {
        let host = super::super::profile::tests::host_fixture();
        for held in ["windows-answers.iso", "windows-enrolment.iso"] {
            let listing = format!(
                " Type   Device   Target   Source\n\
                 ------------------------------------------------\n \
                 file   disk     sda      /var/lib/libvirt/images/kithara-ci-windows.qcow2\n \
                 file   cdrom    sdb      {}\n \
                 file   cdrom    sdc      {}\n",
                host.cache_root.join("iso/windows-eval.iso").display(),
                host.cache_root.join(held).display(),
            );
            assert_eq!(
                answer_drive(&listing, &host).as_deref(),
                Some("sdc"),
                "{held}"
            );
        }
    }

    /// A drive's source is the last column, and a path may hold spaces.
    /// Read only up to the first one, it names some other file, and that is
    /// the file a rebuild would delete.
    #[test]
    fn a_drive_is_known_by_its_whole_path() {
        let listing = " Type   Device   Target   Source\n\
                       ------------------------------------------------\n \
                       file   disk     sda      /srv/kithara ci/kithara-ci-windows.qcow2\n";

        let sources: Vec<PathBuf> = drives(listing)
            .into_iter()
            .map(|drive| drive.source)
            .collect();

        assert_eq!(
            sources,
            [PathBuf::from("/srv/kithara ci/kithara-ci-windows.qcow2")]
        );
    }

    /// The suite opens the machine's default audio output, and Windows has one
    /// only for a sound card it can see. A guest created without one fails
    /// every test that plays through it, and nothing before the suite says why.
    #[test]
    fn the_guest_is_created_with_a_sound_card() {
        let host = super::super::profile::tests::host_fixture();
        let guest = host.windows.clone().unwrap();

        let args = creation_args(
            &host,
            &guest,
            Path::new("/iso/windows.iso"),
            Path::new("/iso/answers.iso"),
        )
        .unwrap();

        let model = args
            .iter()
            .position(|arg| arg == "--sound")
            .and_then(|at| args.get(at + 1));
        assert!(
            model.is_some_and(|model| model.starts_with("model=")),
            "{args:?}"
        );
    }

    /// Whether the guest exists is libvirt's listing to answer. A state read
    /// that fails says nothing about that: the daemon may be down, or the
    /// caller not allowed to ask.
    #[test]
    fn health_reads_the_state_of_a_guest_libvirt_lists_and_of_no_other() {
        let host = super::super::profile::tests::host_fixture();
        let guest = host.windows.clone().unwrap();
        for (listing, asked) in [("other\n".to_owned(), false), (guest.name.clone(), true)] {
            let process = Process::recording(
                Path::new("/"),
                Recording::default().with_reply(&["virsh", "list"], &listing),
            );

            health(&process, &host).unwrap();

            let steps = process.recorded().unwrap();
            assert_eq!(
                steps
                    .steps()
                    .iter()
                    .any(|step| step.args.iter().any(|arg| arg == "domstate")),
                asked,
                "{listing:?}: {:?}",
                steps.steps()
            );
        }
    }

    #[test]
    fn a_host_without_the_guest_has_nothing_to_remove() {
        let directory = tempfile::tempdir().unwrap();
        let mut host = super::super::profile::tests::host_fixture();
        host.cache_root = directory.path().to_path_buf();
        let guest = host.windows.clone().unwrap();
        let process = Process::recording(
            directory.path(),
            Recording::default().with_reply(&["virsh", "list"], "other\n"),
        );

        remove(&process, &host, &guest).unwrap();

        let steps = process.recorded().unwrap();
        assert!(
            steps
                .steps()
                .iter()
                .all(|step| !step.args.iter().any(|arg| arg == "undefine")),
            "{:?}",
            steps.steps()
        );
    }

    /// The guest signs itself in on every restart with a password nobody
    /// types, and Windows expires a local password after 42 days. Once it
    /// had, the sign-in failed and the runner, which starts in that session,
    /// never came back. Provisioning ends in a restart, so the account is
    /// settled before it runs.
    #[test]
    fn the_account_the_guest_signs_in_as_never_has_its_password_expire() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits inside the workspace");
        let text = fs::read_to_string(root.join(consts::GUEST_ANSWER_FILE)).unwrap();
        let answer = roxmltree::Document::parse(&text).unwrap();
        let text_of = |node: roxmltree::Node<'_, '_>, name: &str| {
            node.children()
                .find(|child| child.has_tag_name(name))
                .and_then(|child| child.text())
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let user = answer
            .descendants()
            .find(|node| node.has_tag_name("AutoLogon"))
            .map(|logon| text_of(logon, "Username"))
            .expect("the guest signs itself in");
        let mut commands: Vec<(u32, String)> = answer
            .descendants()
            .filter(|node| node.has_tag_name("FirstLogonCommands"))
            .flat_map(|list| list.children().filter(roxmltree::Node::is_element))
            .map(|command| {
                (
                    text_of(command, "Order").parse().unwrap(),
                    text_of(command, "CommandLine"),
                )
            })
            .collect();
        commands.sort();

        let provision = Path::new(consts::GUEST_PROVISION_SCRIPT)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap();
        let provisions = commands
            .iter()
            .position(|(_, line)| line.contains(provision))
            .expect("the first sign-in provisions the guest");
        let settles = commands
            .iter()
            .position(|(_, line)| {
                line.contains(&format!(
                    "Set-LocalUser -Name {user} -PasswordNeverExpires $true"
                ))
            })
            .unwrap_or_else(|| {
                panic!("nothing keeps `{user}`'s password from expiring: {commands:?}")
            });
        assert!(
            settles < provisions,
            "the password is settled after provisioning restarts the guest: {commands:?}"
        );
    }
}
