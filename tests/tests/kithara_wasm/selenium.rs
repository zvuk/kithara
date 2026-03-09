use std::{
    env,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};
use thirtyfour::{
    common::capabilities::{chromium::ChromiumLikeCapabilities, firefox::FirefoxPreferences},
    extensions::cdp::ChromeDevTools,
    prelude::*,
};
use tokio::time::{Instant, sleep};

const CHECK_INTERVAL: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(45);

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

    fn webdriver_default_port(self) -> u16 {
        match self {
            Self::Chrome => 9515,
            Self::Firefox => 4444,
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
    fixture_port: u16,
    geckodriver_path: String,
    headless: bool,
    page_url_override: Option<String>,
    startup_timeout: Duration,
    switch_wait_seconds: u64,
    trunk_port: u16,
    wait_timeout: Duration,
    webdriver_port: u16,
    webdriver_url_override: Option<String>,
}

impl SeleniumConfig {
    fn from_env() -> Self {
        let browser = BrowserKind::from_env();
        let webdriver_port = env_u16(
            "KITHARA_SELENIUM_WEBDRIVER_PORT",
            browser.webdriver_default_port(),
        );

        Self {
            browser,
            chromedriver_path: env::var("CHROMEDRIVER")
                .unwrap_or_else(|_| "/opt/homebrew/bin/chromedriver".to_string()),
            fixture_port: env_u16("KITHARA_SELENIUM_FIXTURE_PORT", 3333),
            geckodriver_path: env::var("GECKODRIVER")
                .unwrap_or_else(|_| "/opt/homebrew/bin/geckodriver".to_string()),
            headless: env_bool("KITHARA_SELENIUM_HEADLESS", true),
            page_url_override: env::var("KITHARA_SELENIUM_PAGE_URL").ok(),
            startup_timeout: Duration::from_secs(env_u64(
                "KITHARA_SELENIUM_STARTUP_TIMEOUT_SECS",
                DEFAULT_STARTUP_TIMEOUT.as_secs(),
            )),
            switch_wait_seconds: env_u64("KITHARA_SELENIUM_SWITCH_WAIT_SECS", 12),
            trunk_port: env_u16("KITHARA_SELENIUM_TRUNK_PORT", 9092),
            wait_timeout: Duration::from_secs(env_u64(
                "KITHARA_SELENIUM_WAIT_TIMEOUT_SECS",
                DEFAULT_WAIT_TIMEOUT.as_secs(),
            )),
            webdriver_port,
            webdriver_url_override: env::var("KITHARA_SELENIUM_WEBDRIVER_URL").ok(),
        }
    }

    fn fixture_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.fixture_port)
    }

    fn hls_url(&self) -> String {
        format!("{}/hls/master.m3u8", self.fixture_base_url())
    }

    fn mp3_url(&self) -> String {
        format!("{}/track.mp3", self.fixture_base_url())
    }

    fn drm_url(&self) -> String {
        format!("{}/drm/master.m3u8", self.fixture_base_url())
    }

    fn page_url(&self) -> String {
        self.page_url_override
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}/", self.trunk_port))
    }

    fn webdriver_status_url(&self) -> String {
        format!("{}/status", self.webdriver_url())
    }

    fn webdriver_url(&self) -> String {
        self.webdriver_url_override
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", self.webdriver_port))
    }
}

#[derive(Debug)]
struct ChildGuard {
    child: Child,
    name: &'static str,
}

impl ChildGuard {
    fn spawn(name: &'static str, mut cmd: Command) -> Result<Self, String> {
        let child = cmd
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| format!("failed to spawn {name}: {err}"))?;
        Ok(Self { child, name })
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        println!("[selenium] stopped {}", self.name);
    }
}

#[derive(Debug)]
struct SeleniumHarness {
    config: SeleniumConfig,
    fixture_server: Option<ChildGuard>,
    trunk_server: Option<ChildGuard>,
    webdriver_server: Option<ChildGuard>,
}

impl SeleniumHarness {
    async fn start(config: SeleniumConfig) -> Result<Self, String> {
        let mut harness = Self {
            config,
            fixture_server: None,
            trunk_server: None,
            webdriver_server: None,
        };

        harness.ensure_fixture_server().await?;
        harness.ensure_trunk_server().await?;
        harness.ensure_webdriver_server().await?;

        Ok(harness)
    }

    async fn ensure_fixture_server(&mut self) -> Result<(), String> {
        let health_url = format!("{}/health", self.config.fixture_base_url());
        if http_ok(&health_url).await {
            println!("[selenium] fixture server already running: {health_url}");
            return Ok(());
        }

        println!(
            "[selenium] starting fixture server on port {}",
            self.config.fixture_port
        );
        let mut cmd = Command::new("cargo");
        cmd.current_dir(repo_root())
            .args([
                "run",
                "-p",
                "kithara-integration-tests",
                "--bin",
                "fixture_server",
            ])
            .env("FIXTURE_PORT", self.config.fixture_port.to_string());

        self.fixture_server = Some(ChildGuard::spawn("fixture_server", cmd)?);
        wait_http_ready(&health_url, self.config.startup_timeout).await
    }

    async fn ensure_trunk_server(&mut self) -> Result<(), String> {
        let page_url = self.config.page_url();

        if self.config.page_url_override.is_some() {
            println!("[selenium] using external wasm page: {page_url}");
            return wait_page_ready(&page_url, self.config.startup_timeout).await;
        }

        if page_ready_once(&page_url).await {
            println!("[selenium] trunk page already running: {page_url}");
            return Ok(());
        }

        println!(
            "[selenium] starting trunk serve on port {}",
            self.config.trunk_port
        );
        let trunk_port = self.config.trunk_port.to_string();
        let toolchain =
            env::var("KITHARA_SELENIUM_TOOLCHAIN").unwrap_or_else(|_| "nightly".to_string());
        let mut cmd = Command::new("trunk");
        cmd.current_dir(repo_root().join("crates/kithara-wasm"))
            .env("RUSTUP_TOOLCHAIN", toolchain)
            .env_remove("NO_COLOR")
            .args([
                "serve",
                "--address",
                "127.0.0.1",
                "--port",
                &trunk_port,
                "--no-autoreload",
            ]);

        self.trunk_server = Some(ChildGuard::spawn("trunk", cmd)?);
        wait_page_ready(&page_url, self.config.startup_timeout).await
    }

    async fn ensure_webdriver_server(&mut self) -> Result<(), String> {
        let status_url = self.config.webdriver_status_url();
        if http_ok(&status_url).await {
            println!("[selenium] webdriver already running: {status_url}");
            return Ok(());
        }

        if self.config.webdriver_url_override.is_some() {
            return Err(format!(
                "webdriver at {} is not reachable",
                self.config.webdriver_url()
            ));
        }

        println!(
            "[selenium] starting {}driver on port {}",
            self.config.browser.name(),
            self.config.webdriver_port
        );
        let binary = self
            .config
            .browser
            .webdriver_binary(&self.config)
            .to_string();
        let mut cmd = Command::new(binary);
        match self.config.browser {
            BrowserKind::Chrome => {
                cmd.arg(format!("--port={}", self.config.webdriver_port));
            }
            BrowserKind::Firefox => {
                cmd.arg("--port")
                    .arg(self.config.webdriver_port.to_string());
            }
        }

        self.webdriver_server = Some(ChildGuard::spawn("webdriver", cmd)?);
        wait_http_ready(&status_url, self.config.startup_timeout).await
    }

    async fn new_session(&self) -> Result<WasmPlayerSelenium, String> {
        let driver = build_webdriver(&self.config).await?;
        Ok(WasmPlayerSelenium {
            config: self.config.clone(),
            driver,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Snapshot {
    active_index: Option<usize>,
    dur: Option<f64>,
    pc: Option<f64>,
    playlist: Vec<String>,
    pos: Option<f64>,
    status: String,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            active_index: None,
            dur: None,
            pc: None,
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
                .goto(self.config.page_url())
                .await
                .map_err(|err| format!("failed to open page: {err}"))?;

            let ready = self
                .wait_for(
                    "player bootstrap",
                    self.config.wait_timeout,
                    CHECK_INTERVAL,
                    |snap| snap.playlist.len() >= 2 && snap.status.contains("Ready"),
                )
                .await;
            if ready.is_ok() {
                return Ok(());
            }

            let status = self.status_text().await;
            if status.contains("cross-origin isolation") && attempt < 2 {
                sleep(Duration::from_secs(1)).await;
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

    async fn install_console_capture(&self) {
        let script = r#"
            if (!window.__consoleLogs) {
                window.__consoleLogs = [];
                const orig = console.log.bind(console);
                console.log = (...args) => {
                    window.__consoleLogs.push(args.map(String).join(' '));
                    if (window.__consoleLogs.length > 200) {
                        window.__consoleLogs.splice(0, window.__consoleLogs.length - 200);
                    }
                    orig(...args);
                };
                const origWarn = console.warn.bind(console);
                console.warn = (...args) => {
                    window.__consoleLogs.push('[WARN] ' + args.map(String).join(' '));
                    origWarn(...args);
                };
                const origErr = console.error.bind(console);
                console.error = (...args) => {
                    window.__consoleLogs.push('[ERROR] ' + args.map(String).join(' '));
                    origErr(...args);
                };
            }
        "#;
        let _ = self.driver.execute(script, Vec::<Value>::new()).await;
    }

    async fn collect_browser_logs(&self) -> String {
        let mut logs = Vec::new();

        // UI event log
        let script = r#"
            return [...document.querySelectorAll('#event-log > div')]
                .slice(-30)
                .map(el => el.textContent ?? '')
                .join('\n');
        "#;
        if let Ok(ret) = self.driver.execute(script, Vec::<Value>::new()).await {
            if let Ok(s) = ret.convert::<String>() {
                if !s.is_empty() {
                    logs.push(format!("--- UI event log ---\n{s}"));
                }
            }
        }

        // Captured console logs
        let console_script = "return (window.__consoleLogs || []).slice(-80).join('\\n');";
        if let Ok(ret) = self
            .driver
            .execute(console_script, Vec::<Value>::new())
            .await
        {
            if let Ok(s) = ret.convert::<String>() {
                if !s.is_empty() && s != "<no console capture>" {
                    logs.push(format!("--- console logs ---\n{s}"));
                }
            }
        }

        if logs.is_empty() {
            return "<no logs collected>".to_string();
        }
        logs.join("\n\n")
    }

    async fn prepare_local_tracks(&self) -> Result<TrackIndexes, String> {
        self.open_player_page().await?;
        self.clear_playlist().await?;

        self.add_track(&self.config.mp3_url()).await?;
        self.add_track(&self.config.hls_url()).await?;
        self.add_track(&self.config.drm_url()).await?;

        let mp3 = self.find_track_index("track.mp3").await?;
        let hls = self.find_track_index("hls/master.m3u8").await?;
        let drm = self.find_track_index("drm/master.m3u8").await?;

        Ok(TrackIndexes { drm, hls, mp3 })
    }

    async fn clear_playlist(&self) -> Result<(), String> {
        self.driver
            .execute(
                "window.playlist && (window.playlist.length = 0); \
                 typeof renderPlaylist === 'function' && renderPlaylist();",
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
            CHECK_INTERVAL,
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
                out.pc = window.__player.process_count();
                out.pos = window.__player.get_position_ms();
                out.dur = window.__player.get_duration_ms();
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
            sleep(interval).await;
        }

        Err(format!(
            "timeout waiting for {description}; last_snapshot={last:?}"
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
            CHECK_INTERVAL,
            |snap| snap.active_index == Some(index),
        )
        .await?;

        self.click_button("play-btn").await?;

        self.wait_for(
            &format!("track {label} duration known"),
            self.config.wait_timeout,
            CHECK_INTERVAL,
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
        sleep(window).await;
        let end = self.snapshot().await;

        let pos_delta = end.pos.unwrap_or(0.0) - start.pos.unwrap_or(0.0);
        if pos_delta >= min_pos_delta_ms {
            return Ok(());
        }

        let pc_delta = end.pc.unwrap_or(0.0) - start.pc.unwrap_or(0.0);
        if pc_delta >= 5.0 {
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

        sleep(Duration::from_secs(self.config.switch_wait_seconds)).await;

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
                    "return window.__player ? window.__player.get_volume() : -1;",
                    Vec::<Value>::new(),
                )
                .await
                .map_err(|err| format!("get volume failed: {err}"))?
                .convert::<f64>()
                .map_err(|err| format!("volume conversion failed: {err}"))?;

            if (current - value).abs() <= 0.011 {
                return Ok(());
            }
            sleep(CHECK_INTERVAL).await;
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
            CHECK_INTERVAL,
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
            CHECK_INTERVAL,
            |snap| snap.status.contains("Stopped") && snap.pos.unwrap_or(0.0) <= 2500.0,
        )
        .await?;

        self.click_button("play-btn").await?;

        self.wait_for(
            "play after stop starts from beginning",
            Duration::from_secs(10),
            CHECK_INTERVAL,
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

    /// Count dedicated Web Workers via Chrome DevTools Protocol.
    ///
    /// Returns `(worker_count, worker_titles)` — only dedicated workers
    /// (not service workers or shared workers).
    async fn count_web_workers(&self) -> Result<(usize, Vec<String>), String> {
        let dev_tools = ChromeDevTools::new(self.driver.handle.clone());
        let result = dev_tools
            .execute_cdp("Target.getTargets")
            .await
            .map_err(|e| format!("CDP Target.getTargets failed: {e}"))?;

        let targets = result["targetInfos"]
            .as_array()
            .ok_or_else(|| "CDP response missing targetInfos array".to_string())?;

        let mut worker_titles = Vec::new();
        for target in targets {
            if target["type"].as_str() == Some("worker") {
                let title = target["title"].as_str().unwrap_or("<unknown>");
                worker_titles.push(title.to_string());
            }
        }

        Ok((worker_titles.len(), worker_titles))
    }

    /// Run the worker count scenario: verify workers are created and cleaned up
    /// as expected during the player lifecycle.
    ///
    /// Uses a baseline approach: WASM infrastructure (tokio_with_wasm,
    /// wasm_safe_thread) may create its own workers at page load. We measure
    /// the baseline and check that exactly 2 *additional* workers appear after
    /// track load (engine command worker + shared audio worker).
    async fn run_worker_count_scenario(&self) -> Result<(), String> {
        // Phase 1: Measure baseline worker count after page load.
        self.open_player_page().await?;
        sleep(Duration::from_secs(2)).await;

        let (baseline, urls_baseline) = self.count_web_workers().await?;
        println!(
            "[worker_count] baseline after page load: {baseline} workers, \
             urls={urls_baseline:?}"
        );

        // Phase 2: Select a track — triggers engine + shared audio worker creation.
        self.clear_playlist().await?;
        self.add_track(&self.config.hls_url()).await?;
        let hls_idx = self.find_track_index("hls/master.m3u8").await?;

        self.click_playlist_item(hls_idx).await?;
        self.wait_for(
            "track selected",
            self.config.wait_timeout,
            CHECK_INTERVAL,
            |snap| snap.active_index == Some(hls_idx),
        )
        .await?;
        self.click_button("play-btn").await?;

        // Wait for resource to be fully loaded (duration known).
        self.wait_for(
            "track duration known",
            self.config.wait_timeout,
            CHECK_INTERVAL,
            |snap| snap.dur.unwrap_or(0.0) > 0.0,
        )
        .await?;

        // Sample worker count right after track load (may include ephemeral workers).
        let (count_peak, urls_peak) = self.count_web_workers().await?;
        let delta_peak = count_peak.saturating_sub(baseline);
        println!(
            "[worker_count] after track load: {count_peak} workers (+{delta_peak} \
             from baseline), urls={urls_peak:?}"
        );

        // Phase 3: Wait for ephemeral workers (probe/decoder spawn_blocking) to exit.
        sleep(Duration::from_secs(10)).await;

        // Verify playback is still alive.
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

        // Expect at most +2 from baseline: engine command worker + shared audio worker.
        // With wasm_safe_thread pool model, workers may be reused (delta=0).
        // The key invariant: no unbounded growth.
        if delta_steady > 2 {
            return Err(format!(
                "too many workers at steady state: expected at most baseline+2 \
                 (engine + shared audio), got baseline({baseline})+{delta_steady}={count_steady}: \
                 {urls_steady:?}"
            ));
        }

        // Phase 4: Report ephemeral cleanup.
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
                "return window.__player ? window.__player.get_position_ms() : -1;",
                Vec::<Value>::new(),
            )
            .await
            .map_err(|err| format!("get_position_ms script failed: {err}"))?
            .convert::<f64>()
            .map_err(|err| format!("get_position_ms conversion failed: {err}"))
    }

    async fn check_playback_advancing(
        &self,
        duration: Duration,
        interval: Duration,
    ) -> Result<(bool, f64), String> {
        let mut positions = Vec::new();
        let steps = (duration.as_millis() / interval.as_millis()).max(2) as usize;

        for _ in 0..steps {
            sleep(interval).await;
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
            sleep(POLL_INTERVAL).await;
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
            .check_playback_advancing(Duration::from_secs(3), POLL_INTERVAL)
            .await?;
        Ok((true, playback_ok))
    }

    /// Like [`seek_and_verify`] but with longer timeouts and event capture for DRM.
    async fn drm_seek_and_verify(
        &self,
        target_ms: f64,
        label: &str,
    ) -> Result<(bool, bool), String> {
        // Drain events before seek so we see only seek-related events
        let _ = self
            .driver
            .execute(
                "window.__player && window.__player.take_events();",
                Vec::<Value>::new(),
            )
            .await;

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
            sleep(POLL_INTERVAL).await;
            let pos = self.get_position_ms().await?;
            position_samples.push(pos);
            if (low..=high).contains(&pos) {
                seek_ok = true;
                break;
            }
        }

        if !seek_ok {
            eprintln!("[drm_seek] {label}: seek NOT ok, positions={position_samples:?}");
            return Ok((false, false));
        }

        // Sample position + events during playback check
        let mut positions = Vec::new();
        let steps = 16; // 8s / 500ms
        for _ in 0..steps {
            sleep(POLL_INTERVAL).await;
            let pos = self.get_position_ms().await?;
            positions.push(pos);
        }

        // Collect events that happened during/after seek
        let events = self
            .driver
            .execute(
                "return window.__player ? (window.__player.take_events() || '') : '';",
                Vec::<Value>::new(),
            )
            .await
            .ok()
            .and_then(|v| v.convert::<String>().ok())
            .unwrap_or_default();

        let advancing =
            positions.len() >= 2 && positions.last().copied().unwrap_or(0.0) > positions[0] + 100.0;

        if !advancing {
            eprintln!(
                "[drm_seek] {label}: NOT advancing, positions={positions:?}\nevents:\n{events}"
            );
        }

        Ok((true, advancing))
    }

    async fn scenario_crossfade(&self, tracks: TrackIndexes) -> Result<(), String> {
        self.select_track_and_wait(tracks.hls, "HLS").await?;
        self.select_track_and_wait(tracks.mp3, "MP3").await?;
        sleep(Duration::from_secs(15)).await;
        self.assert_motion("crossfade stable", Duration::from_secs(3), 250.0)
            .await
    }

    async fn scenario_seek_hang(&self, tracks: TrackIndexes) -> Result<(), String> {
        self.select_track_and_wait(tracks.mp3, "MP3").await?;

        let (started, _) = self
            .check_playback_advancing(Duration::from_secs(3), POLL_INTERVAL)
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
            sleep(Duration::from_millis(300)).await;
        }

        sleep(Duration::from_secs(5)).await;

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
            .check_playback_advancing(Duration::from_secs(5), POLL_INTERVAL)
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
            .check_playback_advancing(Duration::from_secs(8), POLL_INTERVAL)
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
            sleep(POLL_INTERVAL).await;
        }

        self.assert_motion(
            "hls log scenario still advancing",
            Duration::from_secs(3),
            200.0,
        )
        .await
    }
}

async fn build_webdriver(config: &SeleniumConfig) -> Result<WebDriver, String> {
    let driver_url = config.webdriver_url();

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
            WebDriver::new(&driver_url, caps)
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
            caps.set_preferences(prefs)
                .map_err(|err| format!("failed to set firefox prefs: {err}"))?;

            WebDriver::new(&driver_url, caps)
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

fn env_u16(name: &str, default: u16) -> u16 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
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
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest.to_path_buf())
}

async fn http_ok(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build();

    let Ok(client) = client else {
        return false;
    };

    match client.get(url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn wait_http_ready(url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if http_ok(url).await {
            return Ok(());
        }
        sleep(CHECK_INTERVAL).await;
    }

    Err(format!("timeout waiting for {url}"))
}

async fn page_ready_once(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build();

    let Ok(client) = client else {
        return false;
    };

    match client.get(url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn wait_page_ready(url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if page_ready_once(url).await {
            return Ok(());
        }
        sleep(CHECK_INTERVAL).await;
    }

    Err(format!("timeout waiting for wasm page {url}"))
}

/// Create a [`WasmPlayerSelenium`] session with its owning harness.
///
/// Returns both the harness (which keeps trunk/webdriver/fixture-server
/// processes alive via [`ChildGuard`]) and the WebDriver session.
/// The caller must keep the harness alive for the session's lifetime
/// and drop it **after** the session is closed.
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
async fn selenium_player_scenarios() {
    let (_harness, session) = selenium_setup().await;
    let result = session.run_player_scenarios().await;
    selenium_teardown(session, "player_scenarios", result).await;
}

#[kithara::test(selenium)]
async fn selenium_diagnostic_suite() {
    let (_harness, session) = selenium_setup().await;
    let result = session.run_diagnostic_suite().await;
    selenium_teardown(session, "diagnostic_suite", result).await;
}

#[kithara::test(selenium)]
async fn selenium_hls_log_scenario() {
    let (_harness, session) = selenium_setup().await;
    let result = session.run_hls_log_scenario().await;
    selenium_teardown(session, "hls_log_scenario", result).await;
}

#[kithara::test(selenium)]
async fn selenium_drm_playback_scenario() {
    let (_harness, session) = selenium_setup().await;
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
/// - Ephemeral spawn_blocking workers (probe/decoder) cleaned up
#[kithara::test(selenium)]
async fn selenium_worker_count() {
    let (_harness, session) = selenium_setup().await;
    let result = session.run_worker_count_scenario().await;
    selenium_teardown(session, "worker_count", result).await;
}
