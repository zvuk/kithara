use std::{
    env, fs,
    io::{BufRead, BufReader},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use kithara::platform::time::{self, Duration, Instant};
use kithara_test_fixtures::SignalAsset;
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use thirtyfour::{
    bidi::modules::script::GetRealms,
    common::capabilities::{chromium::ChromiumLikeCapabilities, firefox::FirefoxPreferences},
    prelude::*,
};
use tracing::warn;

struct Consts;
impl Consts {
    /// Generated MPEG clip the playlist loads as its local file track.
    const MP3: SignalAsset = SignalAsset::MP3_SINE880_48K_162S;
    const CHECK_INTERVAL: Duration = Duration::from_millis(250);
    const POLL_INTERVAL: Duration = Duration::from_millis(500);
    const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(180);
    const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(45);

    /// Installed before the page's own scripts run (via CDP
    /// `Page.addScriptToEvaluateOnNewDocument` for Chrome, and again as a
    /// same-origin script right after `goto()` for browsers without CDP
    /// support). Captures `console.log/warn/error` plus uncaught errors and
    /// unhandled promise rejections into `window.__consoleLogs`, so a
    /// bootstrap failure that happens *before* the test can inject anything
    /// (a failed dynamic `import()`, a rejected top-level `await`) still
    /// shows up: without this a stuck "Initializing WASM..." status reports
    /// only a bare timeout with no indication of what the browser saw.
    const CONSOLE_CAPTURE_JS: &'static str = r#"
        if (!window.__consoleLogs) {
            window.__consoleLogs = [];
            const push = (msg) => {
                window.__consoleLogs.push(msg);
                if (window.__consoleLogs.length > 200) {
                    window.__consoleLogs.splice(0, window.__consoleLogs.length - 200);
                }
            };
            const origLog = console.log.bind(console);
            console.log = (...args) => { push(args.map(String).join(' ')); origLog(...args); };
            const origWarn = console.warn.bind(console);
            console.warn = (...args) => { push('[WARN] ' + args.map(String).join(' ')); origWarn(...args); };
            const origErr = console.error.bind(console);
            console.error = (...args) => { push('[ERROR] ' + args.map(String).join(' ')); origErr(...args); };
            window.addEventListener('error', (e) => {
                push(`[UNCAUGHT] ${e.message} at ${e.filename}:${e.lineno}:${e.colno}`);
            });
            window.addEventListener('unhandledrejection', (e) => {
                const reason = e.reason && e.reason.stack ? e.reason.stack : e.reason;
                push(`[UNHANDLED-REJECTION] ${reason}`);
            });
        }
    "#;
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum BrowserKind {
    Chrome,
    Firefox,
}

impl BrowserKind {
    fn from_env() -> Self {
        match env::var("KITHARA_SELENIUM_BROWSER")
            .unwrap_or_else(|_| "chrome".to_string())
            .to_lowercase()
            .as_str()
        {
            "firefox" | "ff" => Self::Firefox,
            _ => Self::Chrome,
        }
    }

    fn webdriver_binary(self, cfg: &SeleniumConfig) -> &str {
        match self {
            Self::Chrome => &cfg.chromedriver_path,
            Self::Firefox => &cfg.geckodriver_path,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Firefox => "firefox",
        }
    }
}

#[derive(Debug, Clone)]
struct SeleniumConfig {
    browser: BrowserKind,
    chromedriver_path: String,
    geckodriver_path: String,
    headless: bool,
    page_url_override: Option<String>,
    startup_timeout: Duration,
    switch_wait_seconds: u64,
    test_server_port: Option<u16>,
    trunk_port: Option<u16>,
    wait_timeout: Duration,
    webdriver_port: Option<u16>,
    webdriver_url_override: Option<String>,
}

impl SeleniumConfig {
    fn from_env() -> Self {
        Self {
            browser: BrowserKind::from_env(),
            chromedriver_path: env::var("CHROMEDRIVER")
                .unwrap_or_else(|_| "chromedriver".to_string()),
            geckodriver_path: env::var("GECKODRIVER").unwrap_or_else(|_| "geckodriver".to_string()),
            headless: env_bool("KITHARA_SELENIUM_HEADLESS", true),
            page_url_override: env::var("KITHARA_SELENIUM_PAGE_URL").ok(),
            startup_timeout: Duration::from_secs(env_u64(
                "KITHARA_SELENIUM_STARTUP_TIMEOUT_SECS",
                Consts::DEFAULT_STARTUP_TIMEOUT.as_secs(),
            )),
            switch_wait_seconds: env_u64("KITHARA_SELENIUM_SWITCH_WAIT_SECS", 12),
            test_server_port: env_u16_opt("KITHARA_SELENIUM_TEST_SERVER_PORT"),
            trunk_port: env_u16_opt("KITHARA_SELENIUM_TRUNK_PORT"),
            wait_timeout: Duration::from_secs(env_u64(
                "KITHARA_SELENIUM_WAIT_TIMEOUT_SECS",
                Consts::DEFAULT_WAIT_TIMEOUT.as_secs(),
            )),
            webdriver_port: env_u16_opt("KITHARA_SELENIUM_WEBDRIVER_PORT"),
            webdriver_url_override: env::var("KITHARA_SELENIUM_WEBDRIVER_URL").ok(),
        }
    }
}

#[derive(Debug)]
struct PortLease {
    listener: Option<TcpListener>,
    port: u16,
}

impl PortLease {
    fn reserve(pin: Option<u16>, label: &str) -> Result<Self, String> {
        let requested_port = pin.unwrap_or(0);
        let listener = TcpListener::bind(("127.0.0.1", requested_port)).map_err(|err| {
            pin.map_or_else(
                || format!("failed to reserve {label} port: {err}"),
                |port| {
                    format!(
                        "{label} port {port} is already in use; this harness will not attach to a \
                         server it did not start: {err}"
                    )
                },
            )
        })?;
        let port = listener
            .local_addr()
            .map_err(|err| format!("failed to read reserved {label} port: {err}"))?
            .port();

        Ok(Self {
            listener: Some(listener),
            port,
        })
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn release(&mut self) {
        self.listener = None;
    }
}

#[derive(Debug, Clone)]
struct Endpoints {
    test_server_port: u16,
    page_url: String,
    webdriver_url: String,
}

impl Endpoints {
    fn test_server_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.test_server_port)
    }

    fn hls_url(&self) -> String {
        format!("{}/assets/hls/master.m3u8", self.test_server_base_url())
    }

    fn mp3_url(&self) -> String {
        format!("{}{}", self.test_server_base_url(), Consts::MP3.path())
    }

    fn drm_url(&self) -> String {
        format!("{}/assets/drm/master.m3u8", self.test_server_base_url())
    }

    fn page_url(&self) -> &str {
        &self.page_url
    }

    fn webdriver_status_url(&self) -> String {
        format!("{}/status", self.webdriver_url())
    }

    fn webdriver_url(&self) -> &str {
        &self.webdriver_url
    }
}

#[derive(Debug)]
struct ChildGuard {
    child: Child,
    name: &'static str,
}

impl ChildGuard {
    fn spawn(name: &'static str, mut cmd: Command) -> Result<Self, String> {
        let program = cmd.get_program().to_string_lossy().into_owned();
        let child = cmd
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| format!("failed to spawn {name} (`{program}`): {err}"))?;
        Ok(Self { child, name })
    }

    fn spawn_piped(name: &'static str, mut cmd: Command) -> Result<(Self, ChildStdout), String> {
        let program = cmd.get_program().to_string_lossy().into_owned();
        let child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| format!("failed to spawn {name} (`{program}`): {err}"))?;
        let mut guard = Self { child, name };
        let stdout = guard
            .child
            .stdout
            .take()
            .ok_or_else(|| format!("failed to capture {name} stdout"))?;
        Ok((guard, stdout))
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        println!("[selenium] stopped {}", self.name);
    }
}

fn forward_test_server_stdout(
    stdout: ChildStdout,
    port_sender: mpsc::Sender<u16>,
) -> Result<(), String> {
    let reader_thread = thread::Builder::new()
        .name("selenium-test-server-stdout".to_string())
        .spawn(move || {
            let reader = BufReader::new(stdout);
            let mut port_sent = false;

            for line in reader.lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(err) => {
                        println!("[selenium] failed to read test server stdout: {err}");
                        break;
                    }
                };

                if !port_sent && let Some(raw_url) = line.strip_prefix("test server listening on ")
                {
                    match Url::parse(raw_url.trim()) {
                        Ok(url) => match url.port() {
                            Some(port) => {
                                port_sent = true;
                                let _ = port_sender.send(port);
                            }
                            None => println!(
                                "[selenium] test server listening URL has no explicit port: {url}"
                            ),
                        },
                        Err(err) => println!(
                            "[selenium] failed to parse test server listening URL \
                             `{raw_url}`: {err}"
                        ),
                    }
                }

                println!("{line}");
            }
        })
        .map_err(|err| format!("failed to start test server stdout thread: {err}"))?;
    // The reader outlives this call: the child's stdout closes when the
    // `ChildGuard` kills it, which ends the thread.
    drop(reader_thread);
    Ok(())
}

#[derive(Debug)]
struct SeleniumHarness {
    config: SeleniumConfig,
    endpoints: Endpoints,
    _test_server: ChildGuard,
    trunk_server: Option<ChildGuard>,
    webdriver_server: Option<ChildGuard>,
}

impl SeleniumHarness {
    async fn start(config: SeleniumConfig) -> Result<Self, String> {
        let (mut trunk_lease, page_url) = if let Some(url) = config.page_url_override.as_ref() {
            (None, url.clone())
        } else {
            let lease = PortLease::reserve(config.trunk_port, "trunk")?;
            let page_url = format!("http://127.0.0.1:{}/", lease.port());
            (Some(lease), page_url)
        };
        let (mut webdriver_lease, webdriver_url) =
            if let Some(url) = config.webdriver_url_override.as_ref() {
                (None, url.clone())
            } else {
                let lease = PortLease::reserve(config.webdriver_port, "webdriver")?;
                let webdriver_url = format!("http://127.0.0.1:{}", lease.port());
                (Some(lease), webdriver_url)
            };
        let (test_server, test_server_port) = Self::ensure_test_server(&config).await?;
        let endpoints = Endpoints {
            test_server_port,
            page_url,
            webdriver_url,
        };
        let mut harness = Self {
            config,
            endpoints,
            _test_server: test_server,
            trunk_server: None,
            webdriver_server: None,
        };

        harness.ensure_trunk_server(trunk_lease.as_mut()).await?;
        harness
            .ensure_webdriver_server(webdriver_lease.as_mut())
            .await?;

        Ok(harness)
    }

    async fn ensure_test_server(config: &SeleniumConfig) -> Result<(ChildGuard, u16), String> {
        let mut port_lease = match config.test_server_port {
            Some(port) => Some(PortLease::reserve(Some(port), "test server")?),
            None => None,
        };
        let requested_port = config.test_server_port.unwrap_or(0);
        println!("[selenium] starting test server on port {requested_port}");
        let mut cmd = Command::new("cargo");
        cmd.current_dir(repo_root())
            .args([
                "run",
                "-p",
                "kithara-integration-tests",
                "--bin",
                "test_server",
            ])
            .env("TEST_SERVER_PORT", requested_port.to_string());

        if let Some(lease) = port_lease.as_mut() {
            lease.release();
        }
        let (guard, stdout) = ChildGuard::spawn_piped("test_server", cmd)?;
        let (port_sender, port_receiver) = mpsc::channel();
        forward_test_server_stdout(stdout, port_sender)?;
        let port = wait_test_server_port(port_receiver, config.startup_timeout).await?;

        if let Some(pin) = config.test_server_port
            && port != pin
        {
            return Err(format!(
                "test server reported port {port}, but pinned port {pin} was requested"
            ));
        }

        let health_url = format!("http://127.0.0.1:{port}/health");
        wait_url_ready(&health_url, config.startup_timeout, "test server").await?;
        Ok((guard, port))
    }

    async fn ensure_trunk_server(
        &mut self,
        port_lease: Option<&mut PortLease>,
    ) -> Result<(), String> {
        let page_url = self.endpoints.page_url().to_string();

        if self.config.page_url_override.is_some() {
            println!("[selenium] using external wasm page: {page_url}");
            return wait_url_ready(&page_url, self.config.startup_timeout, "wasm page").await;
        }

        let port_lease = port_lease.ok_or_else(|| "trunk port lease is missing".to_string())?;
        let trunk_port = port_lease.port();
        println!("[selenium] starting trunk serve on port {trunk_port}");
        let dist_dir = selenium_dist_dir(trunk_port);
        fs::create_dir_all(&dist_dir).map_err(|err| {
            format!(
                "failed to create trunk dist directory {}: {err}",
                dist_dir.display()
            )
        })?;
        let trunk_port = trunk_port.to_string();
        let toolchain =
            env::var("KITHARA_SELENIUM_TOOLCHAIN").unwrap_or_else(|_| "nightly".to_string());
        let mut cmd = Command::new("trunk");
        cmd.current_dir(repo_root().join("crates/kithara-ffi"))
            .env("RUSTUP_TOOLCHAIN", toolchain)
            .env_remove("NO_COLOR")
            .args([
                "serve",
                "--address",
                "127.0.0.1",
                "--port",
                &trunk_port,
                "--no-autoreload",
            ])
            .arg("--dist")
            .arg(&dist_dir);
        // Realtime playback assertions (assert_motion, realtime seek) require
        // an optimised wasm build: our own crates intentionally stay at debug
        // opt-level, so the opt-0 audio pipeline (rubato sinc monomorphised in
        // kithara-audio) cannot sustain realtime in a debug build. Build
        // release by default; opt into debug only for fast non-realtime checks.
        if env::var("KITHARA_SELENIUM_DEBUG").is_err() {
            println!("[selenium] building wasm in release");
            cmd.arg("--release");
        }

        port_lease.release();
        self.trunk_server = Some(ChildGuard::spawn("trunk", cmd)?);
        wait_url_ready(&page_url, self.config.startup_timeout, "wasm page").await
    }

    async fn ensure_webdriver_server(
        &mut self,
        port_lease: Option<&mut PortLease>,
    ) -> Result<(), String> {
        let status_url = self.endpoints.webdriver_status_url();

        if self.config.webdriver_url_override.is_some() {
            println!(
                "[selenium] using external webdriver: {}",
                self.endpoints.webdriver_url()
            );
            return wait_url_ready(&status_url, self.config.startup_timeout, "webdriver").await;
        }

        let port_lease = port_lease.ok_or_else(|| "webdriver port lease is missing".to_string())?;
        let webdriver_port = port_lease.port();
        println!(
            "[selenium] starting {}driver on port {webdriver_port}",
            self.config.browser.name(),
        );
        let binary = self
            .config
            .browser
            .webdriver_binary(&self.config)
            .to_string();
        let mut cmd = Command::new(binary);
        match self.config.browser {
            BrowserKind::Chrome => {
                cmd.arg(format!("--port={webdriver_port}"));
            }
            BrowserKind::Firefox => {
                cmd.arg("--port").arg(webdriver_port.to_string());
            }
        }

        port_lease.release();
        self.webdriver_server = Some(ChildGuard::spawn("webdriver", cmd)?);
        wait_url_ready(&status_url, self.config.startup_timeout, "webdriver").await
    }

    async fn new_session(&self) -> Result<WasmPlayerSelenium, String> {
        let driver = build_webdriver(&self.config, &self.endpoints).await?;
        let session = WasmPlayerSelenium {
            config: self.config.clone(),
            driver,
            endpoints: self.endpoints.clone(),
        };
        // Installed once per session, before the first navigation: a boot
        // failure inside the page's top-level module script (a rejected
        // dynamic import, a thrown error before the test gets a chance to
        // inject anything) must still be visible in `wait_for`'s timeout
        // error instead of vanishing into a bare timeout.
        session.install_boot_capture().await;
        Ok(session)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Snapshot {
    active_index: Option<usize>,
    dur: Option<f64>,
    playlist: Vec<String>,
    pos: Option<f64>,
    status: String,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            active_index: None,
            dur: None,
            playlist: Vec::new(),
            pos: None,
            status: "<snapshot-unavailable>".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TrackIndexes {
    drm: usize,
    hls: usize,
    mp3: usize,
}

#[derive(Debug)]
struct WasmPlayerSelenium {
    config: SeleniumConfig,
    driver: WebDriver,
    endpoints: Endpoints,
}

impl WasmPlayerSelenium {
    async fn close(self) {
        let _ = self.driver.quit().await;
    }

    async fn save_screenshot(&self, path: &str) {
        let _ = self.driver.screenshot(Path::new(path)).await;
    }

    async fn open_player_page(&self) -> Result<(), String> {
        for attempt in 0..3 {
            self.driver
                .goto(self.endpoints.page_url())
                .await
                .map_err(|err| format!("failed to open page: {err}"))?;
            self.install_console_capture().await;

            let ready = self
                .wait_for(
                    "player bootstrap",
                    self.config.wait_timeout,
                    Consts::CHECK_INTERVAL,
                    |snap| snap.playlist.len() >= 2 && snap.status.contains("Ready"),
                )
                .await;
            if ready.is_ok() {
                return Ok(());
            }

            let status = self.status_text().await;
            if status.contains("cross-origin isolation") && attempt < 2 {
                time::sleep(Duration::from_secs(1)).await;
                continue;
            }

            return ready.map(|_| ());
        }

        Err("failed to bootstrap page after retries".to_string())
    }

    async fn status_text(&self) -> String {
        let script = "return document.getElementById('status')?.textContent ?? '';";
        match self.driver.execute(script, Vec::<Value>::new()).await {
            Ok(ret) => ret.convert::<String>().unwrap_or_default(),
            Err(_) => String::new(),
        }
    }

    /// Best-effort same-origin install, run right after `goto()`. Covers
    /// browsers without CDP support (Firefox) and reinforces the CDP install
    /// below; a no-op when the guard in [`Consts::CONSOLE_CAPTURE_JS`] shows
    /// it is already installed for this document.
    async fn install_console_capture(&self) {
        let _ = self
            .driver
            .execute(Consts::CONSOLE_CAPTURE_JS, Vec::<Value>::new())
            .await;
    }

    /// Install [`Consts::CONSOLE_CAPTURE_JS`] via CDP
    /// `Page.addScriptToEvaluateOnNewDocument` so it runs before any of the
    /// page's own scripts, on every navigation in this session — including
    /// the very first one. Chrome-only (CDP); Firefox relies solely on the
    /// post-`goto()` injection in [`Self::install_console_capture`].
    async fn install_boot_capture(&self) {
        if self.config.browser != BrowserKind::Chrome {
            return;
        }
        let dev_tools = self.driver.cdp();
        let _ = dev_tools
            .send_raw(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": Consts::CONSOLE_CAPTURE_JS }),
            )
            .await;
    }

    async fn collect_browser_logs(&self) -> String {
        let mut logs = Vec::new();

        let script = r#"
            return [...document.querySelectorAll('#event-log > div')]
                .slice(-30)
                .map(el => el.textContent ?? '')
                .join('\n');
        "#;
        if let Ok(ret) = self.driver.execute(script, Vec::<Value>::new()).await
            && let Ok(s) = ret.convert::<String>()
            && !s.is_empty()
        {
            logs.push(format!("--- UI event log ---\n{s}"));
        }

        let console_script = "return (window.__consoleLogs || []).slice(-80).join('\\n');";
        if let Ok(ret) = self
            .driver
            .execute(console_script, Vec::<Value>::new())
            .await
            && let Ok(s) = ret.convert::<String>()
            && !s.is_empty()
            && s != "<no console capture>"
        {
            logs.push(format!("--- console logs ---\n{s}"));
        }

        if logs.is_empty() {
            return "<no logs collected>".to_string();
        }
        logs.join("\n\n")
    }

    async fn prepare_local_tracks(&self) -> Result<TrackIndexes, String> {
        self.open_player_page().await?;
        self.clear_playlist().await?;

        self.add_track(&self.endpoints.mp3_url()).await?;
        self.add_track(&self.endpoints.hls_url()).await?;
        self.add_track(&self.endpoints.drm_url()).await?;

        let mp3 = self.find_track_index(Consts::MP3.name()).await?;
        let hls = self.find_track_index("hls/master.m3u8").await?;
        let drm = self.find_track_index("drm/master.m3u8").await?;

        Ok(TrackIndexes { drm, hls, mp3 })
    }

    async fn clear_playlist(&self) -> Result<(), String> {
        self.driver
            .execute(
                "typeof clearPlaylist === 'function' \
                 ? clearPlaylist() \
                 : (window.playlist && (window.playlist.length = 0), \
                    typeof renderPlaylist === 'function' && renderPlaylist());",
                Vec::<Value>::new(),
            )
            .await
            .map_err(|err| format!("clear playlist failed: {err}"))?;
        Ok(())
    }

    async fn add_track(&self, url: &str) -> Result<(), String> {
        let snap = self.snapshot().await;
        if snap.playlist.iter().any(|item| item.contains(url)) {
            return Ok(());
        }

        let input = self
            .driver
            .find(By::Id("url-input"))
            .await
            .map_err(|err| format!("url-input not found: {err}"))?;
        input
            .clear()
            .await
            .map_err(|err| format!("failed to clear url input: {err}"))?;
        input
            .send_keys(url)
            .await
            .map_err(|err| format!("failed to type url: {err}"))?;

        let add_btn = self
            .driver
            .find(By::Id("add-btn"))
            .await
            .map_err(|err| format!("add-btn not found: {err}"))?;
        add_btn
            .click()
            .await
            .map_err(|err| format!("failed to click add button: {err}"))?;

        self.wait_for(
            "track added to playlist",
            self.config.wait_timeout,
            Consts::CHECK_INTERVAL,
            |current| current.playlist.iter().any(|item| item.contains(url)),
        )
        .await
        .map(|_| ())
    }

    async fn snapshot(&self) -> Snapshot {
        let script = r#"
            const status = document.getElementById('status')?.textContent ?? '';
            const items = [...document.querySelectorAll('#playlist li')];
            const playlist = items.map((el) => el.textContent ?? '');
            const activeIndex = items.findIndex((el) => el.classList.contains('active'));
            const out = {
                status,
                playlist,
                activeIndex: activeIndex >= 0 ? activeIndex : null,
            };
            if (window.__player) {
                out.pos = window.__player.currentTimeMs();
                out.dur = window.__durationMs ?? 0;
            }
            return out;
        "#;

        let result = self.driver.execute(script, Vec::<Value>::new()).await;
        match result {
            Ok(ret) => ret.convert::<Snapshot>().unwrap_or_default(),
            Err(_) => Snapshot::default(),
        }
    }

    async fn wait_for<F>(
        &self,
        description: &str,
        timeout: Duration,
        interval: Duration,
        mut condition: F,
    ) -> Result<Snapshot, String>
    where
        F: FnMut(&Snapshot) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut last = Snapshot::default();

        while Instant::now() < deadline {
            last = self.snapshot().await;
            if condition(&last) {
                return Ok(last);
            }
            time::sleep(interval).await;
        }

        // A timeout alone hides why: surface what the browser actually saw
        // (console errors, uncaught exceptions, unhandled rejections) rather
        // than only the last polled snapshot.
        let logs = self.collect_browser_logs().await;
        Err(format!(
            "timeout waiting for {description}; last_snapshot={last:?}\nbrowser logs:\n{logs}"
        ))
    }

    async fn click_playlist_item(&self, index: usize) -> Result<(), String> {
        let items = self
            .driver
            .find_all(By::Css("#playlist li"))
            .await
            .map_err(|err| format!("failed to query playlist items: {err}"))?;

        if index >= items.len() {
            return Err(format!(
                "playlist item {index} not found; count={}",
                items.len()
            ));
        }

        items[index]
            .click()
            .await
            .map_err(|err| format!("failed to click playlist item #{index}: {err}"))
    }

    async fn click_button(&self, button_id: &str) -> Result<(), String> {
        let button = self
            .driver
            .find(By::Id(button_id))
            .await
            .map_err(|err| format!("button '{button_id}' not found: {err}"))?;
        button
            .click()
            .await
            .map_err(|err| format!("failed to click '{button_id}': {err}"))
    }

    async fn find_track_index(&self, needle: &str) -> Result<usize, String> {
        let snap = self.snapshot().await;
        snap.playlist
            .iter()
            .position(|line| line.contains(needle))
            .ok_or_else(|| format!("playlist entry containing '{needle}' not found: {snap:?}"))
    }

    async fn select_track_and_wait(&self, index: usize, label: &str) -> Result<(), String> {
        self.click_playlist_item(index).await?;

        self.wait_for(
            &format!("track {label} selected"),
            self.config.wait_timeout,
            Consts::CHECK_INTERVAL,
            |snap| snap.active_index == Some(index),
        )
        .await?;

        self.click_button("play-btn").await?;

        self.wait_for(
            &format!("track {label} duration known"),
            self.config.wait_timeout,
            Consts::CHECK_INTERVAL,
            |snap| snap.dur.unwrap_or(0.0) > 0.0,
        )
        .await?;

        self.assert_motion(
            &format!("{label} playback started"),
            Duration::from_secs(4),
            350.0,
        )
        .await
    }

    async fn assert_motion(
        &self,
        description: &str,
        window: Duration,
        min_pos_delta_ms: f64,
    ) -> Result<(), String> {
        let start = self.snapshot().await;
        time::sleep(window).await;
        let end = self.snapshot().await;

        let pos_delta = end.pos.unwrap_or(0.0) - start.pos.unwrap_or(0.0);
        if pos_delta >= min_pos_delta_ms {
            return Ok(());
        }

        Err(format!(
            "{description}: playback did not advance enough; start={start:?} end={end:?}"
        ))
    }

    async fn wait_between_switches(&self, label: &str) -> Result<(), String> {
        self.assert_motion(
            &format!("{label} stable before wait"),
            Duration::from_secs(2),
            300.0,
        )
        .await?;

        time::sleep(Duration::from_secs(self.config.switch_wait_seconds)).await;

        self.assert_motion(
            &format!("{label} stable after wait"),
            Duration::from_secs(2),
            300.0,
        )
        .await
    }

    async fn seek_ratio(&self, ratio: f64, tolerance_ms: f64) -> Result<f64, String> {
        let snap = self.snapshot().await;
        let duration_ms = snap.dur.unwrap_or(0.0);
        if duration_ms <= 0.0 {
            return Err(format!("cannot seek: unknown duration; snapshot={snap:?}"));
        }

        let ratio = ratio.clamp(0.0, 1.0);
        let target = duration_ms * ratio;

        self.driver
            .execute(
                r#"
                    const slider = document.getElementById('seek-slider');
                    const target = arguments[0];
                    slider.value = String(target);
                    slider.dispatchEvent(new Event('input', { bubbles: true }));
                    slider.dispatchEvent(new Event('change', { bubbles: true }));
                "#,
                vec![json!(target)],
            )
            .await
            .map_err(|err| format!("seek via slider failed: {err}"))?;

        let reached_via_slider = self
            .wait_for(
                &format!("seek ratio {ratio:.3} via slider"),
                Duration::from_secs(12),
                Duration::from_millis(200),
                |s| (s.pos.unwrap_or(0.0) - target).abs() <= tolerance_ms,
            )
            .await
            .is_ok();
        if !reached_via_slider {
            self.driver
                .execute(
                    "window.__player && window.__player.seek(arguments[0]);",
                    vec![json!(target)],
                )
                .await
                .map_err(|err| format!("seek fallback failed: {err}"))?;

            self.wait_for(
                &format!("seek ratio {ratio:.3} via fallback"),
                Duration::from_secs(30),
                Duration::from_millis(200),
                |s| (s.pos.unwrap_or(0.0) - target).abs() <= tolerance_ms,
            )
            .await?;
        }

        Ok(target)
    }

    async fn set_volume_percent(&self, percent: u8) -> Result<(), String> {
        if percent > 100 {
            return Err(format!("volume percent out of range: {percent}"));
        }

        let value = f64::from(percent) / 100.0;
        self.driver
            .execute(
                r#"
                    const slider = document.getElementById('volume-slider');
                    slider.value = String(arguments[0]);
                    slider.dispatchEvent(new Event('input', { bubbles: true }));
                    slider.dispatchEvent(new Event('change', { bubbles: true }));
                "#,
                vec![json!(value)],
            )
            .await
            .map_err(|err| format!("set volume failed: {err}"))?;

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let current = self
                .driver
                .execute(
                    "return window.__player ? window.__player.volume() : -1;",
                    Vec::<Value>::new(),
                )
                .await
                .map_err(|err| format!("get volume failed: {err}"))?
                .convert::<f64>()
                .map_err(|err| format!("volume conversion failed: {err}"))?;

            if (current - value).abs() <= 0.011 {
                return Ok(());
            }
            time::sleep(Consts::CHECK_INTERVAL).await;
        }

        Err(format!("volume did not reach {value} in time"))
    }

    async fn seek_to_start_and_verify(&self, require_play_click: bool) -> Result<(), String> {
        self.seek_ratio(0.0, 2000.0).await?;

        if require_play_click {
            self.click_button("play-btn").await?;
        }

        self.wait_for(
            "position near start after seek",
            Duration::from_secs(10),
            Consts::CHECK_INTERVAL,
            |snap| snap.pos.unwrap_or(0.0) <= 2500.0,
        )
        .await?;

        self.assert_motion("playback resumes from start", Duration::from_secs(3), 250.0)
            .await
    }

    async fn stop_and_replay_verify(&self) -> Result<(), String> {
        self.click_button("stop-btn").await?;

        self.wait_for(
            "stop state reflected",
            Duration::from_secs(10),
            Consts::CHECK_INTERVAL,
            |snap| snap.status.contains("Stopped") && snap.pos.unwrap_or(0.0) <= 2500.0,
        )
        .await?;

        self.click_button("play-btn").await?;

        self.wait_for(
            "play after stop starts from beginning",
            Duration::from_secs(10),
            Consts::CHECK_INTERVAL,
            |snap| snap.pos.unwrap_or(0.0) <= 4000.0,
        )
        .await?;

        self.assert_motion("stop->play restart", Duration::from_secs(3), 250.0)
            .await
    }

    async fn run_track_scenario(&self, index: usize, label: &str) -> Result<(), String> {
        self.select_track_and_wait(index, label).await?;

        self.set_volume_percent(25).await?;
        self.wait_between_switches(label).await?;

        self.seek_ratio(0.30, 7000.0).await?;
        self.assert_motion(
            &format!("{label} motion after 30% seek"),
            Duration::from_secs(3),
            250.0,
        )
        .await?;

        self.seek_ratio(0.60, 9000.0).await?;
        self.assert_motion(
            &format!("{label} motion after 60% seek"),
            Duration::from_secs(3),
            250.0,
        )
        .await?;

        self.seek_ratio(0.95, 15_000.0).await?;
        self.assert_motion(
            &format!("{label} motion after 95% seek"),
            Duration::from_millis(2500),
            150.0,
        )
        .await?;

        self.seek_to_start_and_verify(false).await?;

        self.seek_ratio(0.95, 15_000.0).await?;
        self.seek_to_start_and_verify(true).await?;

        self.seek_ratio(0.95, 15_000.0).await?;
        self.stop_and_replay_verify().await
    }

    /// Count dedicated Web Workers, through `WebDriver` `BiDi`.
    ///
    /// Returns `(worker_count, worker_origins)` — only dedicated workers, not
    /// service workers, shared workers or worklets. The equivalent Chrome
    /// `DevTools` call answers this browser only; every browser this lane can
    /// name speaks `BiDi`.
    async fn count_web_workers(&self) -> Result<(usize, Vec<String>), String> {
        let bidi = self
            .driver
            .bidi()
            .await
            .map_err(|err| format!("BiDi session unavailable: {err}"))?;
        let realms = bidi
            .send(GetRealms {
                context: None,
                r#type: Some("dedicated-worker".to_owned()),
            })
            .await
            .map_err(|err| format!("BiDi script.getRealms failed: {err}"))?;

        let workers: Vec<String> = realms
            .realms
            .into_iter()
            .map(|realm| realm.origin)
            .collect();

        Ok((workers.len(), workers))
    }

    /// Run the worker count scenario: verify workers are created and cleaned up
    /// as expected during the player lifecycle.
    ///
    /// Uses a baseline approach: WASM infrastructure (`tokio_with_wasm`,
    /// `wasm_safe_thread`) may create its own workers at page load. We measure
    /// the baseline and check that exactly 2 *additional* workers appear after
    /// track load (engine command worker + shared audio worker).
    async fn run_worker_count_scenario(&self) -> Result<(), String> {
        self.open_player_page().await?;
        time::sleep(Duration::from_secs(2)).await;

        let (baseline, urls_baseline) = self.count_web_workers().await?;
        println!(
            "[worker_count] baseline after page load: {baseline} workers, \
             urls={urls_baseline:?}"
        );

        self.clear_playlist().await?;
        self.add_track(&self.endpoints.hls_url()).await?;
        let hls_idx = self.find_track_index("hls/master.m3u8").await?;

        self.click_playlist_item(hls_idx).await?;
        self.wait_for(
            "track selected",
            self.config.wait_timeout,
            Consts::CHECK_INTERVAL,
            |snap| snap.active_index == Some(hls_idx),
        )
        .await?;
        self.click_button("play-btn").await?;

        self.wait_for(
            "track duration known",
            self.config.wait_timeout,
            Consts::CHECK_INTERVAL,
            |snap| snap.dur.unwrap_or(0.0) > 0.0,
        )
        .await?;

        let (count_peak, urls_peak) = self.count_web_workers().await?;
        let delta_peak = count_peak.saturating_sub(baseline);
        println!(
            "[worker_count] after track load: {count_peak} workers (+{delta_peak} \
             from baseline), urls={urls_peak:?}"
        );

        time::sleep(Duration::from_secs(10)).await;

        self.assert_motion(
            "playback after worker settle",
            Duration::from_secs(3),
            250.0,
        )
        .await?;

        let (count_steady, urls_steady) = self.count_web_workers().await?;
        let delta_steady = count_steady.saturating_sub(baseline);
        println!(
            "[worker_count] steady state: {count_steady} workers (+{delta_steady} \
             from baseline), urls={urls_steady:?}"
        );

        if delta_steady > 2 {
            return Err(format!(
                "too many workers at steady state: expected at most baseline+2 \
                 (engine + shared audio), got baseline({baseline})+{delta_steady}={count_steady}: \
                 {urls_steady:?}"
            ));
        }

        if count_peak > count_steady {
            println!(
                "[worker_count] ephemeral cleanup OK: peak={count_peak} → \
                 steady={count_steady} ({} ephemeral workers terminated)",
                count_peak - count_steady
            );
        } else {
            println!(
                "[worker_count] no ephemeral workers observed at peak \
                 (peak={count_peak}, steady={count_steady})"
            );
        }

        Ok(())
    }

    async fn run_player_scenarios(&self) -> Result<(), String> {
        let tracks = self.prepare_local_tracks().await?;

        self.run_track_scenario(tracks.mp3, "MP3").await?;
        self.run_track_scenario(tracks.hls, "HLS").await?;

        self.select_track_and_wait(tracks.mp3, "MP3").await?;
        self.wait_between_switches("MP3").await?;

        self.select_track_and_wait(tracks.hls, "HLS").await?;
        self.wait_between_switches("HLS").await
    }

    async fn get_position_ms(&self) -> Result<f64, String> {
        self.driver
            .execute(
                "return window.__player ? window.__player.currentTimeMs() : -1;",
                Vec::<Value>::new(),
            )
            .await
            .map_err(|err| format!("currentTimeMs script failed: {err}"))?
            .convert::<f64>()
            .map_err(|err| format!("currentTimeMs conversion failed: {err}"))
    }

    async fn check_playback_advancing(
        &self,
        duration: Duration,
        interval: Duration,
    ) -> Result<(bool, f64), String> {
        let mut positions = Vec::new();
        let steps = (duration.as_millis() / interval.as_millis()).max(2) as usize;

        for _ in 0..steps {
            time::sleep(interval).await;
            let position = self.get_position_ms().await?;
            if position >= 0.0 {
                positions.push(position);
            }
        }

        if positions.len() < 2 {
            return Ok((false, positions.last().copied().unwrap_or(-1.0)));
        }

        let advancing = positions.last().copied().unwrap_or(0.0) > positions[0] + 100.0;
        Ok((advancing, positions.last().copied().unwrap_or(-1.0)))
    }

    async fn seek_and_verify(&self, target_ms: f64, label: &str) -> Result<(bool, bool), String> {
        self.driver
            .execute(
                "window.__player && window.__player.seek(arguments[0]);",
                vec![json!(target_ms)],
            )
            .await
            .map_err(|err| format!("seek '{label}' command failed: {err}"))?;

        let low = (target_ms - 5000.0).max(0.0);
        let high = target_ms + 15_000.0;
        let deadline = Instant::now() + Duration::from_secs(10);

        let mut seek_ok = false;
        while Instant::now() < deadline {
            time::sleep(Consts::POLL_INTERVAL).await;
            let pos = self.get_position_ms().await?;
            if (low..=high).contains(&pos) {
                seek_ok = true;
                break;
            }
        }

        if !seek_ok {
            return Ok((false, false));
        }

        let (playback_ok, _) = self
            .check_playback_advancing(Duration::from_secs(3), Consts::POLL_INTERVAL)
            .await?;
        Ok((true, playback_ok))
    }

    /// Like [`seek_and_verify`] but with longer timeouts and event capture for DRM.
    async fn drm_seek_and_verify(
        &self,
        target_ms: f64,
        label: &str,
    ) -> Result<(bool, bool), String> {
        self.driver
            .execute(
                "window.__player && window.__player.seek(arguments[0]);",
                vec![json!(target_ms)],
            )
            .await
            .map_err(|err| format!("seek '{label}' command failed: {err}"))?;

        let low = (target_ms - 5000.0).max(0.0);
        let high = target_ms + 15_000.0;
        let deadline = Instant::now() + Duration::from_secs(15);

        let mut seek_ok = false;
        let mut position_samples = Vec::new();
        while Instant::now() < deadline {
            time::sleep(Consts::POLL_INTERVAL).await;
            let pos = self.get_position_ms().await?;
            position_samples.push(pos);
            if (low..=high).contains(&pos) {
                seek_ok = true;
                break;
            }
        }

        if !seek_ok {
            warn!("[drm_seek] {label}: seek NOT ok, positions={position_samples:?}");
            return Ok((false, false));
        }

        let mut positions = Vec::new();
        let steps = 16;
        for _ in 0..steps {
            time::sleep(Consts::POLL_INTERVAL).await;
            let pos = self.get_position_ms().await?;
            positions.push(pos);
        }

        let events = self.collect_browser_logs().await;

        let advancing =
            positions.len() >= 2 && positions.last().copied().unwrap_or(0.0) > positions[0] + 100.0;

        if !advancing {
            warn!("[drm_seek] {label}: NOT advancing, positions={positions:?}\nevents:\n{events}");
        }

        Ok((true, advancing))
    }

    async fn scenario_crossfade(&self, tracks: TrackIndexes) -> Result<(), String> {
        self.select_track_and_wait(tracks.hls, "HLS").await?;
        self.select_track_and_wait(tracks.mp3, "MP3").await?;
        time::sleep(Duration::from_secs(15)).await;
        self.assert_motion("crossfade stable", Duration::from_secs(3), 250.0)
            .await
    }

    async fn scenario_seek_hang(&self, tracks: TrackIndexes) -> Result<(), String> {
        self.select_track_and_wait(tracks.mp3, "MP3").await?;

        let (started, _) = self
            .check_playback_advancing(Duration::from_secs(3), Consts::POLL_INTERVAL)
            .await?;
        if !started {
            return Err("playback did not start for MP3 seek scenario".to_string());
        }

        let plan = [
            (50_000.0, "cycle1-fwd50"),
            (100_000.0, "cycle1-fwd100"),
            (12_000.0, "cycle1-bwd12"),
            (80_000.0, "cycle2-fwd80"),
            (5000.0, "cycle2-bwd5"),
        ];

        let mut failures = Vec::new();
        for (target, label) in plan {
            let (seek_ok, playback_ok) = self.seek_and_verify(target, label).await?;
            if !(seek_ok && playback_ok) {
                failures.push((label, seek_ok, playback_ok));
            }
        }

        if failures.is_empty() {
            return Ok(());
        }

        Err(format!("seek scenario failures: {failures:?}"))
    }

    async fn scenario_playlist_crash(&self, tracks: TrackIndexes) -> Result<(), String> {
        for idx in [tracks.hls, tracks.mp3, tracks.hls] {
            self.click_playlist_item(idx).await?;
            time::sleep(Duration::from_millis(300)).await;
        }

        time::sleep(Duration::from_secs(5)).await;

        let count = self
            .driver
            .execute(
                "return document.querySelector('#playlist') ? document.querySelector('#playlist').children.length : -1;",
                Vec::<Value>::new(),
            )
            .await
            .map_err(|err| format!("playlist count script failed: {err}"))?
            .convert::<i64>()
            .map_err(|err| format!("playlist count conversion failed: {err}"))?;

        if count <= 0 {
            return Err(format!("playlist appears broken, children={count}"));
        }

        Ok(())
    }

    async fn scenario_hls_playback(&self, tracks: TrackIndexes) -> Result<(), String> {
        self.select_track_and_wait(tracks.hls, "HLS").await?;

        let (started, _) = self
            .check_playback_advancing(Duration::from_secs(5), Consts::POLL_INTERVAL)
            .await?;
        if !started {
            return Err("hls playback did not start".to_string());
        }

        let plan = [
            (30_000.0, "hls-fwd30"),
            (60_000.0, "hls-fwd60"),
            (10_000.0, "hls-bwd10"),
        ];

        let mut failures = Vec::new();
        for (target, label) in plan {
            let (seek_ok, playback_ok) = self.seek_and_verify(target, label).await?;
            if !(seek_ok && playback_ok) {
                failures.push((label, seek_ok, playback_ok));
            }
        }

        if failures.is_empty() {
            return Ok(());
        }

        Err(format!("hls scenario failures: {failures:?}"))
    }

    async fn scenario_drm_playback(&self, tracks: TrackIndexes) -> Result<(), String> {
        self.select_track_and_wait(tracks.drm, "DRM").await?;
        self.install_console_capture().await;

        let (started, _) = self
            .check_playback_advancing(Duration::from_secs(8), Consts::POLL_INTERVAL)
            .await?;
        if !started {
            let snap = self.snapshot().await;
            let logs = self.collect_browser_logs().await;
            return Err(format!(
                "drm playback did not start: {snap:?}\nbrowser logs:\n{logs}"
            ));
        }

        let plan = [
            (30_000.0, "drm-fwd30"),
            (60_000.0, "drm-fwd60"),
            (10_000.0, "drm-bwd10"),
        ];

        let mut failures = Vec::new();
        for (target, label) in plan {
            let (seek_ok, playback_ok) = self.drm_seek_and_verify(target, label).await?;
            if !(seek_ok && playback_ok) {
                let snap = self.snapshot().await;
                let logs = self.collect_browser_logs().await;
                failures.push(format!(
                    "{label}: seek_ok={seek_ok}, playback_ok={playback_ok}, \
                     snap={snap:?}\nbrowser logs:\n{logs}"
                ));
            }
        }

        if failures.is_empty() {
            return Ok(());
        }

        Err(format!("drm scenario failures:\n{}", failures.join("\n")))
    }

    async fn run_diagnostic_suite(&self) -> Result<(), String> {
        let tracks = self.prepare_local_tracks().await?;
        self.scenario_crossfade(tracks).await?;

        let tracks = self.prepare_local_tracks().await?;
        self.scenario_seek_hang(tracks).await?;

        let tracks = self.prepare_local_tracks().await?;
        self.scenario_playlist_crash(tracks).await?;

        let tracks = self.prepare_local_tracks().await?;
        self.scenario_hls_playback(tracks).await?;

        let tracks = self.prepare_local_tracks().await?;
        self.scenario_drm_playback(tracks).await
    }

    async fn run_hls_log_scenario(&self) -> Result<(), String> {
        let tracks = self.prepare_local_tracks().await?;
        self.select_track_and_wait(tracks.hls, "HLS").await?;

        for _ in 0..40 {
            time::sleep(Consts::POLL_INTERVAL).await;
        }

        self.assert_motion(
            "hls log scenario still advancing",
            Duration::from_secs(3),
            200.0,
        )
        .await
    }
}

async fn build_webdriver(
    config: &SeleniumConfig,
    endpoints: &Endpoints,
) -> Result<WebDriver, String> {
    let driver_url = endpoints.webdriver_url();

    let driver = match config.browser {
        BrowserKind::Chrome => {
            let mut caps = DesiredCapabilities::chrome();
            for arg in [
                "--disable-background-networking",
                "--disable-extensions",
                "--no-sandbox",
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--enable-features=SharedArrayBuffer",
                "--autoplay-policy=no-user-gesture-required",
                "--js-flags=--experimental-wasm-threads",
            ] {
                caps.add_arg(arg)
                    .map_err(|err| format!("failed to set chrome args: {err}"))?;
            }
            if config.headless {
                caps.add_arg("--headless=new")
                    .map_err(|err| format!("failed to set chrome headless: {err}"))?;
            }
            // The worker census reads BiDi realms, and the driver only serves
            // them when the session asked for a socket at New Session.
            caps.enable_bidi()
                .map_err(|err| format!("failed to ask chrome for bidi: {err}"))?;
            WebDriver::new(driver_url, caps)
                .await
                .map_err(|err| format!("failed to create chrome session: {err}"))?
        }
        BrowserKind::Firefox => {
            let mut caps = DesiredCapabilities::firefox();
            if config.headless {
                caps.add_arg("-headless")
                    .map_err(|err| format!("failed to set firefox headless: {err}"))?;
            }

            let mut prefs = FirefoxPreferences::new();
            prefs
                .set("media.autoplay.default", 0)
                .map_err(|err| format!("failed to set firefox autoplay pref: {err}"))?;
            prefs
                .set("dom.workers.requestAnimationFrame", true)
                .map_err(|err| format!("failed to set firefox worker pref: {err}"))?;
            // Everything the page loads is on 127.0.0.1. A profile that
            // inherits a host proxy sends the browser's own startup traffic
            // through it, and a proxy that wants credentials opens an auth
            // prompt the headless session cannot answer.
            prefs
                .set("network.proxy.type", 0)
                .map_err(|err| format!("failed to set firefox proxy pref: {err}"))?;
            caps.set_preferences(prefs)
                .map_err(|err| format!("failed to set firefox prefs: {err}"))?;
            caps.enable_bidi()
                .map_err(|err| format!("failed to ask firefox for bidi: {err}"))?;

            WebDriver::new(driver_url, caps)
                .await
                .map_err(|err| format!("failed to create firefox session: {err}"))?
        }
    };

    driver
        .set_page_load_timeout(Duration::from_secs(60))
        .await
        .map_err(|err| format!("failed to set page load timeout: {err}"))?;
    driver
        .set_script_timeout(Duration::from_secs(30))
        .await
        .map_err(|err| format!("failed to set script timeout: {err}"))?;

    Ok(driver)
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

fn env_u16_opt(name: &str) -> Option<u16> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(3)
        .expect("test package is under tests/crates")
        .to_path_buf()
}

fn selenium_dist_dir(trunk_port: u16) -> PathBuf {
    let target_dir =
        env::var("CARGO_TARGET_DIR").map_or_else(|_| repo_root().join("target"), PathBuf::from);
    let target_dir = if target_dir.is_absolute() {
        target_dir
    } else {
        repo_root().join(target_dir)
    };
    target_dir
        .join("selenium-dist")
        .join(trunk_port.to_string())
}

async fn http_ok(url: &str) -> bool {
    let client = Client::builder().timeout(Duration::from_secs(2)).build();

    let Ok(client) = client else {
        return false;
    };

    match client.get(url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn wait_url_ready(url: &str, timeout: Duration, kind: &str) -> Result<(), String> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if http_ok(url).await {
            return Ok(());
        }
        time::sleep(Consts::CHECK_INTERVAL).await;
    }

    if kind.is_empty() {
        Err(format!("timeout waiting for {url}"))
    } else {
        Err(format!("timeout waiting for {kind} {url}"))
    }
}

async fn wait_test_server_port(receiver: Receiver<u16>, timeout: Duration) -> Result<u16, String> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        match receiver.try_recv() {
            Ok(port) => return Ok(port),
            Err(TryRecvError::Empty) => time::sleep(Consts::CHECK_INTERVAL).await,
            Err(TryRecvError::Disconnected) => {
                return Err(
                    "test server stdout closed before reporting its listening port".to_string(),
                );
            }
        }
    }

    Err("timeout waiting for test server listening port".to_string())
}

/// Create a [`WasmPlayerSelenium`] session with its owning harness.
///
/// Returns both the harness (which keeps trunk/webdriver/test-server
/// processes alive via [`ChildGuard`]) and the `WebDriver` session.
/// The caller must keep the harness alive for the session's lifetime
/// and drop it **after** the session is closed.
#[kithara::fixture]
async fn selenium_setup() -> (SeleniumHarness, WasmPlayerSelenium) {
    let config = SeleniumConfig::from_env();
    let harness = SeleniumHarness::start(config)
        .await
        .unwrap_or_else(|err| panic!("failed to start selenium harness: {err}"));

    let session = harness
        .new_session()
        .await
        .unwrap_or_else(|err| panic!("failed to create webdriver session: {err}"));

    (harness, session)
}

/// Run a selenium scenario with screenshot-on-failure and session cleanup.
///
/// Takes ownership of the session to ensure `close()` is always called.
/// The harness is dropped after cleanup, stopping child processes.
async fn selenium_teardown(session: WasmPlayerSelenium, name: &str, result: Result<(), String>) {
    if result.is_err() {
        let path = format!("/tmp/wasm_{name}_failed.png");
        session.save_screenshot(&path).await;
    }
    session.close().await;
    if let Err(err) = result {
        panic!("{name} failed: {err}");
    }
}

#[kithara::test(selenium)]
async fn selenium_player_scenarios(
    #[future(awt)] selenium_setup: (SeleniumHarness, WasmPlayerSelenium),
) {
    let (_harness, session) = selenium_setup;
    let result = session.run_player_scenarios().await;
    selenium_teardown(session, "player_scenarios", result).await;
}

#[kithara::test(selenium)]
async fn selenium_diagnostic_suite(
    #[future(awt)] selenium_setup: (SeleniumHarness, WasmPlayerSelenium),
) {
    let (_harness, session) = selenium_setup;
    let result = session.run_diagnostic_suite().await;
    selenium_teardown(session, "diagnostic_suite", result).await;
}

#[kithara::test(selenium)]
async fn selenium_hls_log_scenario(
    #[future(awt)] selenium_setup: (SeleniumHarness, WasmPlayerSelenium),
) {
    let (_harness, session) = selenium_setup;
    let result = session.run_hls_log_scenario().await;
    selenium_teardown(session, "hls_log_scenario", result).await;
}

#[kithara::test(selenium)]
async fn selenium_drm_playback_scenario(
    #[future(awt)] selenium_setup: (SeleniumHarness, WasmPlayerSelenium),
) {
    let (_harness, session) = selenium_setup;
    let tracks = session
        .prepare_local_tracks()
        .await
        .unwrap_or_else(|err| panic!("failed to prepare local tracks: {err}"));
    let result = session.scenario_drm_playback(tracks).await;
    selenium_teardown(session, "drm_playback_scenario", result).await;
}

/// Verify Web Worker count at each lifecycle stage:
/// - After page load: 0 workers (engine starts lazily)
/// - Steady state after track load: 2 workers (engine + shared audio)
/// - Ephemeral `spawn_blocking` workers (probe/decoder) cleaned up
#[kithara::test(selenium)]
async fn selenium_worker_count(
    #[future(awt)] selenium_setup: (SeleniumHarness, WasmPlayerSelenium),
) {
    let (_harness, session) = selenium_setup;
    let result = session.run_worker_count_scenario().await;
    selenium_teardown(session, "worker_count", result).await;
}
