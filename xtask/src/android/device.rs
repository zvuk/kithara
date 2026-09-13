//! Android device selection and ownership of emulator processes and reverse ports.

use std::{
    env,
    fmt::Write as _,
    fs,
    io::{ErrorKind, Read as _},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::FileLock;
use sha2::{Digest, Sha256};

use crate::{child, config::AndroidConfig};

struct Consts;

impl Consts {
    const ATTACH_POLL: Duration = Duration::from_millis(500);
    const ATTACH_DEADLINE: Duration = Duration::from_secs(180);
    const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
    const EMULATOR: &'static str = "the emulator this run booted";
    const ORIGIN_PROBE: Duration = Duration::from_secs(5);
}

#[derive(Clone, Copy)]
pub(crate) enum Screen {
    Windowed,
    Headless,
}

impl Screen {
    const fn args(self) -> &'static [&'static str] {
        match self {
            Self::Windowed => &[],
            Self::Headless => &["-no-window"],
        }
    }
}

/// Which device the caller asked for.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Request<'a> {
    Serial(&'a str),
    Avd(&'a str),
    /// The single online device, or the configured default AVD when the host
    /// has none.
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Online {
    pub(crate) serial: String,
    pub(crate) avd: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Someone else started it, so it outlives the run.
    Borrowed(String),
    /// Booted here, and owned for the length of the run.
    Boot(String),
}

pub(crate) struct Selected {
    pub(crate) serial: String,
    adb: PathBuf,
    /// Present only for an emulator this run booted.
    emulator: Option<Child>,
}

impl Selected {
    pub(crate) fn lease(&self) -> Result<FileLock> {
        let home = PathBuf::from(
            env::var_os("HOME").context("HOME is required for Android device leases")?,
        );
        if !home.is_absolute() || !home.is_dir() {
            bail!("HOME must be an existing absolute directory for Android device leases");
        }
        let locks = home.join(".cache/kithara/android-device-locks");
        device_lease(&locks, &self.serial)
    }

    pub(crate) const fn owns_emulator(&self) -> bool {
        self.emulator.is_some()
    }

    pub(crate) fn adb(&self) -> Command {
        let mut command = Command::new(&self.adb);
        command.args(["-s", &self.serial]);
        command
    }

    /// Keep the emulator this run booted running, and stop owning it.
    pub(crate) fn leave_running(&mut self) {
        if self.emulator.take().is_some() {
            println!("==> Emulator {} is left running", self.serial);
        }
    }

    /// Stop the emulator this run booted, and leave every other device alone.
    pub(crate) fn release(mut self) -> Result<()> {
        self.take_down()
    }

    /// Stop only the process group created for this emulator.
    fn take_down(&mut self) -> Result<()> {
        let Some(mut emulator) = self.emulator.take() else {
            return Ok(());
        };
        child::stop(&mut emulator, Consts::EMULATOR).map(drop)
    }
}

impl Drop for Selected {
    fn drop(&mut self) {
        let _ = self.take_down();
    }
}

fn device_lease(directory: &Path, serial: &str) -> Result<FileLock> {
    fs::create_dir_all(directory)?;
    let name = format!(
        "kithara-native-device-{}.lock",
        hex::encode(Sha256::digest(serial.as_bytes()))
    );
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(name))?;
    FileLock::try_exclusive(lock).context("another Android run owns this device")
}

/// A device TCP port forwarded to a port on the host, for the length of one
/// run.
pub(crate) struct Reverse {
    adb: PathBuf,
    serial: String,
    device_port: u16,
}

impl Reverse {
    /// Letting `adb` pick the device port leaves the mappings the device
    /// already carries alone.
    pub(crate) fn create(
        device: &Selected,
        host_port: u16,
        cancel: Option<&child::Cancel>,
    ) -> Result<Self> {
        child::check(cancel)?;
        // Finish acquiring the port before honoring cancellation, so cleanup knows its name.
        let output = control(
            device
                .adb()
                .args(["reverse", "tcp:0", &format!("tcp:{host_port}")]),
            None,
        )
        .context("reverse acquisition outcome unknown; mapping may remain")?;
        if !output.status.success() {
            bail!(
                "adb reverse failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let device_port = parse_reverse_port(&String::from_utf8_lossy(&output.stdout))
            .context("reverse acquisition outcome unknown; mapping may remain")?;
        println!("==> Device port {device_port} reaches the host on {host_port}");
        Ok(Self {
            adb: device.adb.clone(),
            serial: device.serial.clone(),
            device_port,
        })
    }

    pub(crate) const fn device_port(&self) -> u16 {
        self.device_port
    }

    /// Remove this run's mapping, and only this one.
    pub(crate) fn remove(mut self) -> Result<()> {
        self.take_down()
    }

    fn take_down(&mut self) -> Result<()> {
        if self.device_port == 0 {
            return Ok(());
        }
        let port = std::mem::take(&mut self.device_port);
        let output = control(
            Command::new(&self.adb).args([
                "-s",
                &self.serial,
                "reverse",
                "--remove",
                &format!("tcp:{port}"),
            ]),
            None,
        )?;
        output
            .status
            .success()
            .then_some(())
            .context("adb reverse --remove failed")
    }
}

/// Confirm the device can reach the host fixture server through this run's
/// reverse mapping before any test fetches a record.
pub(crate) fn probe_origin(
    device: &Selected,
    device_url: &str,
    cancel: Option<&child::Cancel>,
) -> Result<()> {
    let health = origin_health_url(device_url);
    let port = origin_loopback_port(device_url)?;
    let output = child::output(
        device.adb().args(origin_probe_args(port)),
        cancel,
        Consts::ORIGIN_PROBE,
    )?;
    if origin_probe_reached(&output) {
        return Ok(());
    }
    bail!("{}", origin_probe_failure(&health, &output));
}

fn origin_health_url(device_url: &str) -> String {
    format!("{}/health", device_url.trim_end_matches('/'))
}

fn origin_loopback_port(device_url: &str) -> Result<u16> {
    device_url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("http://127.0.0.1:")
        .and_then(|port| port.parse().ok())
        .filter(|port| *port != 0)
        .context("fixture origin is not an http://127.0.0.1 port")
}

fn origin_probe_args(port: u16) -> [String; 2] {
    [
        "exec-out".into(),
        format!(
            "{{ printf 'GET /health HTTP/1.0\\r\\nHost: 127.0.0.1\\r\\nConnection: close\\r\\n\\r\\n'; toybox sleep 1; }} | toybox nc 127.0.0.1 {port}; echo KITHARA_ORIGIN_PROBE:$?"
        ),
    ]
}

fn origin_probe_reached(output: &std::process::Output) -> bool {
    origin_probe_body(&output.stdout).trim() == "ok"
}

fn origin_probe_body(stdout: &[u8]) -> &str {
    let text = std::str::from_utf8(stdout).unwrap_or("");
    let text = text
        .rsplit_once("KITHARA_ORIGIN_PROBE:")
        .map_or(text, |(body, _)| body);
    match text.rsplit_once("\r\n\r\n") {
        Some((_, body)) => body,
        None => text,
    }
}

fn origin_probe_failure(health: &str, output: &std::process::Output) -> String {
    format!(
        "device cannot reach fixture origin {health} ({status}): stdout={stdout:?} stderr={stderr:?}",
        status = output.status,
        stdout = String::from_utf8_lossy(&output.stdout),
        stderr = String::from_utf8_lossy(&output.stderr),
    )
}

impl Drop for Reverse {
    fn drop(&mut self) {
        let _ = self.take_down();
    }
}

/// `adb reverse tcp:0 tcp:<host>` answers with the device port it allocated.
fn parse_reverse_port(stdout: &str) -> Result<u16> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .context("adb reverse allocated no port")?;
    line.parse()
        .with_context(|| format!("adb reverse answered `{line}` rather than a port"))
}

pub(crate) fn choose(online: &[Online], request: Request<'_>, default_avd: &str) -> Result<Choice> {
    match request {
        Request::Serial(serial) => {
            if online.iter().any(|device| device.serial == serial) {
                return Ok(Choice::Borrowed(serial.to_owned()));
            }
            bail!("device `{serial}` is not online.{}", candidates(online));
        }
        Request::Avd(avd) => Ok(online
            .iter()
            .find(|device| device.avd.as_deref() == Some(avd))
            .map_or_else(|| Choice::Boot(avd.to_owned()), borrowed)),
        Request::Any => match online {
            [] => Ok(Choice::Boot(default_avd.to_owned())),
            [single] => Ok(borrowed(single)),
            several => bail!(
                "several devices are online, so a run would address an arbitrary one. \
                 Name one with `--serial`.{}",
                candidates(several)
            ),
        },
    }
}

fn borrowed(device: &Online) -> Choice {
    Choice::Borrowed(device.serial.clone())
}

fn candidates(online: &[Online]) -> String {
    if online.is_empty() {
        return " No device is attached.".to_owned();
    }
    online
        .iter()
        .fold(" Online:".to_owned(), |mut text, device| {
            let _ = match device.avd.as_deref() {
                Some(avd) => write!(text, "\n  {} ({avd})", device.serial),
                None => write!(text, "\n  {}", device.serial),
            };
            text
        })
}

/// Return only devices adb reports as online.
pub(crate) fn parse_devices(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (serial, state) = line.split_once('\t')?;
            (state.trim() == "device").then(|| serial.trim().to_owned())
        })
        .collect()
}

/// `adb -s <serial> emu avd name` answers with the name and then `OK`.
pub(crate) fn parse_avd_name(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != "OK")
        .map(str::to_owned)
}

pub(crate) fn select(
    adb: &Path,
    emulator: &Path,
    request: Request<'_>,
    screen: Screen,
    android: &AndroidConfig,
    cancel: Option<&child::Cancel>,
) -> Result<Selected> {
    let default_avd = super::require_android_str(&android.default_avd, "default_avd")?;
    let attached = online_devices(adb, cancel)?;
    match choose(&attached, request, default_avd)? {
        Choice::Borrowed(serial) => {
            println!("==> Using device {serial}");
            Ok(Selected {
                serial,
                adb: adb.to_path_buf(),
                emulator: None,
            })
        }
        Choice::Boot(avd) => boot(adb, emulator, &avd, screen, android, cancel),
    }
}

pub(crate) fn control(
    command: &mut Command,
    cancel: Option<&child::Cancel>,
) -> Result<std::process::Output> {
    child::output(command, cancel, Consts::CONTROL_TIMEOUT)
}

fn online_devices(adb: &Path, cancel: Option<&child::Cancel>) -> Result<Vec<Online>> {
    let output = control(Command::new(adb).arg("devices"), cancel)?;
    if !output.status.success() {
        bail!("adb devices failed");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_devices(&stdout)
        .into_iter()
        .map(|serial| {
            let avd = control(
                Command::new(adb).args(["-s", &serial, "emu", "avd", "name"]),
                cancel,
            )
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| parse_avd_name(&String::from_utf8_lossy(&output.stdout)));
            Online { serial, avd }
        })
        .collect())
}

/// Boot the requested AVD and receive its allocated console port.
fn boot(
    adb: &Path,
    emulator: &Path,
    avd: &str,
    screen: Screen,
    android: &AndroidConfig,
    cancel: Option<&child::Cancel>,
) -> Result<Selected> {
    if !emulator.exists() {
        bail!(
            "no device is online and the emulator binary is missing at {}",
            emulator.display()
        );
    }
    require_avd(emulator, avd, cancel)?;

    let report =
        TcpListener::bind(("127.0.0.1", 0)).context("binding emulator console report listener")?;
    report.set_nonblocking(true)?;
    let destination = format!("tcp:{},max=10", report.local_addr()?.port());
    println!("==> Booting AVD '{avd}'");
    child::check(cancel)?;
    let mut child = child::spawn(
        Command::new(emulator)
            .args(["-avd", avd, "-report-console", &destination])
            .args(screen.args()),
    )?;

    let serial = match reported_serial(&report, &mut child, cancel).and_then(|serial| {
        await_serial(adb, &serial, &mut child, cancel)?;
        Ok(serial)
    }) {
        Ok(serial) => serial,
        Err(error) => {
            let stopped = child::stop(&mut child, Consts::EMULATOR);
            return Err(match stopped {
                Ok(_) => error.context("emulator cleanup: ok"),
                Err(stop_error) => error.context(stop_error),
            });
        }
    };

    let mut selected = Selected {
        serial,
        adb: adb.to_path_buf(),
        emulator: Some(child),
    };
    if let Err(error) = await_boot_complete(&mut selected, android, cancel) {
        return Err(match selected.release() {
            Ok(()) => error.context("emulator cleanup: ok"),
            Err(stop_error) => error.context(stop_error),
        });
    }
    Ok(selected)
}

fn require_avd(emulator: &Path, avd: &str, cancel: Option<&child::Cancel>) -> Result<()> {
    let output = control(Command::new(emulator).arg("-list-avds"), cancel)?;
    if !output.status.success() {
        bail!("emulator -list-avds failed");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let known: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if known.contains(&avd) {
        return Ok(());
    }
    bail!(
        "AVD `{avd}` is not installed. Installed:{}",
        known.iter().fold(String::new(), |mut text, name| {
            let _ = write!(text, "\n  {name}");
            text
        })
    );
}

/// The emulator reports its allocated console port to this run's bound listener.
/// Other emulators never receive this endpoint, and no port is guessed from adb.
fn reported_serial(
    report: &TcpListener,
    emulator: &mut Child,
    cancel: Option<&child::Cancel>,
) -> Result<String> {
    let deadline = Instant::now() + Consts::ATTACH_DEADLINE;
    loop {
        child::check(cancel)?;
        require_running(emulator)?;
        match report.accept() {
            Ok((connection, _)) => {
                connection.set_nonblocking(false)?;
                connection.set_read_timeout(Some(Consts::CONTROL_TIMEOUT))?;
                let mut port = String::new();
                connection.take(16).read_to_string(&mut port)?;
                require_running(emulator)?;
                let port: u16 = port
                    .trim()
                    .parse()
                    .context("invalid emulator console port report")?;
                if port == 0 {
                    bail!("emulator reported console port zero");
                }
                return Ok(format!("emulator-{port}"));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => return Err(error).context("receiving emulator console port"),
        }
        if Instant::now() >= deadline {
            bail!("emulator console report timed out");
        }
        thread::sleep(Consts::ATTACH_POLL);
    }
}

fn require_running(emulator: &mut Child) -> Result<()> {
    if let Some(status) = emulator.try_wait().context("checking owned emulator")? {
        bail!("the emulator exited with {status} before it was ready");
    }
    Ok(())
}

fn await_serial(
    adb: &Path,
    serial: &str,
    emulator: &mut Child,
    cancel: Option<&child::Cancel>,
) -> Result<()> {
    let deadline = Instant::now() + Consts::ATTACH_DEADLINE;
    loop {
        child::check(cancel)?;
        require_running(emulator)?;
        let attached = control(Command::new(adb).arg("devices"), cancel)?;
        require_running(emulator)?;
        if attached.status.success()
            && parse_devices(&String::from_utf8_lossy(&attached.stdout))
                .iter()
                .any(|online| online == serial)
        {
            println!("==> Emulator attached as {serial}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "the emulator did not attach as {serial} within {}s",
                Consts::ATTACH_DEADLINE.as_secs()
            );
        }
        thread::sleep(Consts::ATTACH_POLL);
    }
}

/// `adb` sees a device well before the system finishes booting, so the install
/// step would race the package manager.
fn await_boot_complete(
    device: &mut Selected,
    android: &AndroidConfig,
    cancel: Option<&child::Cancel>,
) -> Result<()> {
    let max_attempts = android.boot_wait_attempts.context(
        "ext.android.boot_wait_attempts is not set; fill in the [ext.android] section of .config/xtask.toml",
    )?;
    let poll_interval = Duration::from_secs(android.boot_poll_interval_secs.context(
        "ext.android.boot_poll_interval_secs is not set; fill in the [ext.android] section of .config/xtask.toml",
    )?);

    for _ in 0..max_attempts {
        child::check(cancel)?;
        if let Some(emulator) = &mut device.emulator {
            require_running(emulator)?;
        }
        let output = control(
            device
                .adb()
                .args(["shell", "getprop", "sys.boot_completed"]),
            cancel,
        );
        if let Ok(output) = output
            && output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim() == "1"
        {
            println!("==> Device boot complete");
            return Ok(());
        }
        let deadline = Instant::now() + poll_interval;
        while Instant::now() < deadline {
            child::check(cancel)?;
            thread::sleep(Consts::ATTACH_POLL);
        }
    }
    let timeout_secs = u64::from(max_attempts).saturating_mul(poll_interval.as_secs());
    bail!("device did not finish booting within {timeout_secs} seconds");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn online(serial: &str, avd: Option<&str>) -> Online {
        Online {
            serial: serial.to_owned(),
            avd: avd.map(str::to_owned),
        }
    }

    #[test]
    fn parse_devices_keeps_only_the_ready_ones() {
        let stdout = "List of devices attached\n\
                      emulator-5554\tdevice\n\
                      emulator-5556\toffline\n\
                      RF8N90ABCD\tunauthorized\n\
                      RF8N90WXYZ\tdevice\n";
        assert_eq!(parse_devices(stdout), ["emulator-5554", "RF8N90WXYZ"]);
    }

    #[test]
    fn parse_reverse_port_reads_the_allocated_port() {
        assert_eq!(parse_reverse_port("38833\n").unwrap(), 38_833);
    }

    #[test]
    fn parse_reverse_port_rejects_an_empty_answer() {
        assert!(parse_reverse_port("\n").is_err());
    }

    #[test]
    fn origin_probe_hits_health_on_the_device_loopback() {
        assert_eq!(
            origin_health_url("http://127.0.0.1:38833"),
            "http://127.0.0.1:38833/health"
        );
        assert_eq!(
            origin_loopback_port("http://127.0.0.1:38833").unwrap(),
            38_833
        );
        assert_eq!(
            origin_probe_args(38_833),
            [
                "exec-out".to_owned(),
                "{ printf 'GET /health HTTP/1.0\\r\\nHost: 127.0.0.1\\r\\nConnection: close\\r\\n\\r\\n'; toybox sleep 1; } | toybox nc 127.0.0.1 38833; echo KITHARA_ORIGIN_PROBE:$?".to_owned(),
            ]
        );
        assert_eq!(
            origin_probe_body(
                b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok\nKITHARA_ORIGIN_PROBE:0\n"
            ),
            "ok\n"
        );
        assert!(origin_probe_reached(&std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok".to_vec(),
            stderr: Vec::new(),
        }));
        assert!(
            origin_probe_reached(&std::process::Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(1),
                stdout: b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok".to_vec(),
                stderr: Vec::new(),
            }),
            "nc may exit nonzero after the server closes the socket"
        );
    }

    #[test]
    fn origin_probe_failure_names_the_origin() {
        let output = std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(1),
            stdout: Vec::new(),
            stderr: b"nc: connection refused".to_vec(),
        };
        let message = origin_probe_failure("http://127.0.0.1:38833/health", &output);
        assert!(message.contains("http://127.0.0.1:38833/health"));
        assert!(message.contains("connection refused"));
    }

    #[test]
    fn parse_avd_name_reads_the_name_before_the_acknowledgement() {
        assert_eq!(parse_avd_name("Pixel_4\nOK\n").as_deref(), Some("Pixel_4"));
    }

    #[test]
    fn an_explicit_serial_resolves_to_that_device() {
        let attached = [
            online("emulator-5554", Some("Pixel_4")),
            online("RF8N", None),
        ];
        assert_eq!(
            choose(&attached, Request::Serial("RF8N"), "Kithara_CI").unwrap(),
            Choice::Borrowed("RF8N".to_owned())
        );
    }

    #[test]
    fn an_offline_serial_names_the_candidates_instead_of_running() {
        let attached = [online("emulator-5554", Some("Pixel_4"))];
        let error = choose(&attached, Request::Serial("RF8N"), "Kithara_CI")
            .unwrap_err()
            .to_string();
        assert!(error.contains("RF8N"), "{error}");
        assert!(error.contains("emulator-5554"), "{error}");
    }

    #[test]
    fn an_avd_already_running_is_borrowed_rather_than_booted_again() {
        let attached = [online("emulator-5554", Some("Kithara_CI"))];
        assert_eq!(
            choose(&attached, Request::Avd("Kithara_CI"), "Other").unwrap(),
            Choice::Borrowed("emulator-5554".to_owned())
        );
    }

    #[test]
    fn a_named_avd_is_booted_even_while_another_device_is_online() {
        let attached = [online("emulator-5554", Some("Pixel_4"))];
        assert_eq!(
            choose(&attached, Request::Avd("Kithara_CI"), "Other").unwrap(),
            Choice::Boot("Kithara_CI".to_owned())
        );
    }

    #[test]
    fn a_bare_run_takes_the_single_online_device() {
        let attached = [online("emulator-5554", Some("Pixel_4"))];
        assert_eq!(
            choose(&attached, Request::Any, "Kithara_CI").unwrap(),
            Choice::Borrowed("emulator-5554".to_owned())
        );
    }

    #[test]
    fn a_bare_run_boots_the_configured_avd_when_the_host_is_empty() {
        assert_eq!(
            choose(&[], Request::Any, "Kithara_CI").unwrap(),
            Choice::Boot("Kithara_CI".to_owned())
        );
    }

    #[cfg(unix)]
    mod owned {
        use std::{fs, os::unix::fs::PermissionsExt as _, process::Stdio};

        use super::*;

        fn recording_adb(dir: &Path, trace: &Path, code: i32) -> PathBuf {
            let path = dir.join("adb");
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit {code}\n",
                trace.display()
            );
            fs::write(&path, script).expect("writing the recording adb");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("making the recording adb executable");
            path
        }

        fn recorded(trace: &Path) -> String {
            fs::read_to_string(trace)
                .unwrap_or_default()
                .trim()
                .to_owned()
        }

        fn shell(script: &str) -> Child {
            child::spawn(
                Command::new("sh")
                    .args(["-c", script])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .expect("spawning a shell for the test")
        }

        fn alive(pid: u32) -> bool {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        }

        #[test]
        fn an_exited_emulator_cannot_adopt_an_online_stranger_with_its_serial() {
            let dir = tempfile::tempdir().unwrap();
            let trace = dir.path().join("adb-calls");
            let adb = recording_adb(dir.path(), &trace, 0);
            let mut stranger = shell("exec sleep 30");
            let mut failed = shell("exit 1");
            failed.wait().unwrap();
            let result = await_serial(&adb, "emulator-5554", &mut failed, None);
            assert!(result.unwrap_err().to_string().contains("exited"));
            assert_eq!(recorded(&trace), "");
            assert!(stranger.try_wait().unwrap().is_none());
            child::stop(&mut stranger, "stranger test emulator").unwrap();
        }

        #[test]
        fn concurrent_boots_receive_only_their_own_console_reports() {
            let first = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let second = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            first.set_nonblocking(true).unwrap();
            second.set_nonblocking(true).unwrap();
            assert_ne!(first.local_addr().unwrap(), second.local_addr().unwrap());
            let mut emulator = shell("exec sleep 30");
            for (listener, port) in [(&second, "5558"), (&first, "5556")] {
                let mut report =
                    std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
                std::io::Write::write_all(&mut report, port.as_bytes()).unwrap();
            }
            assert_eq!(
                reported_serial(&first, &mut emulator, None).unwrap(),
                "emulator-5556"
            );
            assert_eq!(
                reported_serial(&second, &mut emulator, None).unwrap(),
                "emulator-5558"
            );
            child::stop(&mut emulator, "test emulator").unwrap();
        }

        #[test]
        fn a_borrowed_device_is_released_without_being_stopped() {
            let dir = tempfile::tempdir().expect("temporary directory");
            let trace = dir.path().join("adb-calls");
            let device = Selected {
                serial: "RF8N90WXYZ".to_owned(),
                adb: recording_adb(dir.path(), &trace, 0),
                emulator: None,
            };

            device.release().unwrap();

            assert_eq!(recorded(&trace), "");
        }

        #[test]
        fn release_stops_only_the_owned_process_without_addressing_a_serial() {
            let dir = tempfile::tempdir().expect("temporary directory");
            let trace = dir.path().join("adb-calls");
            let device = Selected {
                serial: "emulator-5566".to_owned(),
                adb: recording_adb(dir.path(), &trace, 1),
                emulator: Some(shell("exit 0")),
            };

            let started = Instant::now();
            device.release().unwrap();
            assert_eq!(recorded(&trace), "");
            assert!(
                started.elapsed() < Consts::CONTROL_TIMEOUT,
                "release took {:?}",
                started.elapsed()
            );
        }

        #[test]
        fn an_emulator_left_running_survives_every_stop_path() {
            let dir = tempfile::tempdir().expect("temporary directory");
            let trace = dir.path().join("adb-calls");
            let emulator = shell("exec sleep 30");
            let pid = emulator.id();
            let mut device = Selected {
                serial: "emulator-5566".to_owned(),
                adb: recording_adb(dir.path(), &trace, 0),
                emulator: Some(emulator),
            };

            device.leave_running();
            drop(device);

            assert!(alive(pid), "the emulator the run left running was stopped");
            assert_eq!(recorded(&trace), "");
            Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status()
                .expect("stopping the test emulator");
        }

        #[test]
        fn cancellation_during_reverse_acquisition_keeps_the_port_owned() {
            let dir = tempfile::tempdir().unwrap();
            let trace = dir.path().join("adb-calls");
            let adb = recording_adb(dir.path(), &trace, 0);
            fs::write(&adb, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in *tcp:0*) kill -TERM $PPID; echo 41234;; esac\n", trace.display())).unwrap();
            let device = Selected {
                serial: "borrowed-phone".to_owned(),
                adb,
                emulator: None,
            };
            let cancel = child::Cancel::install().unwrap();
            let mapping = Reverse::create(&device, 34567, Some(&cancel)).unwrap();
            assert!(child::check(Some(&cancel)).is_err());
            mapping.remove().unwrap();
            assert_eq!(
                recorded(&trace),
                "-s borrowed-phone reverse tcp:0 tcp:34567\n-s borrowed-phone reverse --remove tcp:41234"
            );
        }

        #[test]
        fn a_hung_reverse_removal_returns_before_the_child_finishes() {
            let dir = tempfile::tempdir().unwrap();
            let trace = dir.path().join("adb-calls");
            let adb = recording_adb(dir.path(), &trace, 0);
            fs::write(&adb, "#!/bin/sh\nsleep 30\n").unwrap();
            let mapping = Reverse {
                adb,
                serial: "borrowed-phone".to_owned(),
                device_port: 41234,
            };
            let started = Instant::now();
            assert!(
                mapping
                    .remove()
                    .unwrap_err()
                    .to_string()
                    .contains("deadline")
            );
            assert!(started.elapsed() < Consts::CONTROL_TIMEOUT + Duration::from_secs(5));
        }

        #[test]
        fn a_reverse_mapping_takes_down_only_its_own_port() {
            let dir = tempfile::tempdir().expect("temporary directory");
            let trace = dir.path().join("adb-calls");
            let mapping = Reverse {
                adb: recording_adb(dir.path(), &trace, 0),
                serial: "emulator-5566".to_owned(),
                device_port: 41_234,
            };

            mapping.remove().unwrap();

            assert_eq!(
                recorded(&trace),
                "-s emulator-5566 reverse --remove tcp:41234"
            );
        }
    }

    #[test]
    fn a_bare_run_refuses_to_pick_between_several_devices() {
        let attached = [
            online("emulator-5554", Some("Pixel_4")),
            online("RF8N", None),
        ];
        let error = choose(&attached, Request::Any, "Kithara_CI")
            .unwrap_err()
            .to_string();
        assert!(error.contains("--serial"), "{error}");
        assert!(error.contains("emulator-5554"), "{error}");
        assert!(error.contains("RF8N"), "{error}");
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;

    #[test]
    fn device_lease_excludes_other_runs_until_cleanup_finishes() {
        let root = tempfile::tempdir().unwrap();
        let first = device_lease(root.path(), "emulator-5554").unwrap();
        assert!(device_lease(root.path(), "emulator-5554").is_err());
        let other = device_lease(root.path(), "emulator-5556").unwrap();
        drop(first);
        let next = device_lease(root.path(), "emulator-5554").unwrap();
        drop((other, next));
    }
}
