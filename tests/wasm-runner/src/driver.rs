//! Drives one headless browser session through a local `WebDriver` binary.

use std::{
    env,
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
};

use anyhow::{Context, Result, bail};
use kithara_platform::time::{Duration, Instant, sleep, timeout};
use kithara_test_utils::free_loopback_port;
use serde_json::{Map, Value, json};

/// How long a freshly spawned driver gets to bind its port.
const DRIVER_START: Duration = Duration::from_secs(5);
/// How long the driver gets to close the browser once the run is over.
const SESSION_CLOSE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Browser {
    Chrome,
    Firefox,
    Safari,
}

impl Browser {
    /// The variable naming the driver binary; `<VAR>_ARGS` adds arguments.
    fn variable(self) -> &'static str {
        match self {
            Self::Chrome => "CHROMEDRIVER",
            Self::Firefox => "GECKODRIVER",
            Self::Safari => "SAFARIDRIVER",
        }
    }

    /// How the page hides its output, per browser, as `wasm-bindgen-test-runner`
    /// chose it.
    pub(crate) fn hidden_style(self) -> &'static str {
        match self {
            Self::Safari => "visibility:hidden",
            Self::Chrome | Self::Firefox => "display:none",
        }
    }
}

/// The driver the environment names, else `chromedriver` from `PATH`.
pub(crate) fn locate() -> Result<(Browser, PathBuf)> {
    for browser in [Browser::Chrome, Browser::Firefox, Browser::Safari] {
        if let Some(path) = env::var_os(browser.variable()).filter(|path| !path.is_empty()) {
            return Ok((browser, path.into()));
        }
    }
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("chromedriver").with_extension(env::consts::EXE_EXTENSION))
        .find(|candidate| candidate.is_file())
        .map(|path| (Browser::Chrome, path))
        .context("no WebDriver: set CHROMEDRIVER, GECKODRIVER or SAFARIDRIVER, or put chromedriver on PATH")
}

/// A running driver and the browser session it opened. `close` ends the
/// session; dropping it only stops the driver, which leaves the browser behind.
pub(crate) struct Session {
    http: reqwest::Client,
    base: String,
    id: String,
    driver: Child,
}

impl Session {
    pub(crate) async fn start(
        browser: Browser,
        path: PathBuf,
        capabilities: Map<String, Value>,
    ) -> Result<Self> {
        let port = free_loopback_port()?;
        let arguments = env::var(format!("{}_ARGS", browser.variable())).unwrap_or_default();
        let driver = Command::new(&path)
            .args(arguments.split_whitespace())
            .arg(format!("--port={port}"))
            .stdout(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {}", path.display()))?;
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let mut session = Self {
            http: reqwest::Client::new(),
            base: format!("http://{address}"),
            id: String::new(),
            driver,
        };
        let deadline = Instant::now() + DRIVER_START;
        while TcpStream::connect(address).is_err() {
            if let Some(status) = session.driver.try_wait()? {
                bail!(
                    "{} exited before binding its port: {status}",
                    path.display()
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "{} did not bind port {port} within {DRIVER_START:?}",
                    path.display()
                );
            }
            sleep(Duration::from_millis(100)).await;
        }
        let reply = session
            .call(
                reqwest::Method::POST,
                "/session",
                Some(new_session(browser, capabilities)),
            )
            .await?;
        session.id = reply
            .get("sessionId")
            .or_else(|| reply.pointer("/value/sessionId"))
            .and_then(Value::as_str)
            .context("the driver answered a new session without an id")?
            .to_owned();
        Ok(session)
    }

    pub(crate) async fn visit(&self, url: &str) -> Result<()> {
        let path = format!("/session/{}/url", self.id);
        self.call(reqwest::Method::POST, &path, Some(json!({ "url": url })))
            .await?;
        Ok(())
    }

    /// The text of element `id` past `offset`, and the offset after it.
    pub(crate) async fn text_since(&self, id: &str, offset: usize) -> Result<(String, usize)> {
        let script = "const el = document.getElementById(arguments[0]); \
                      const text = el ? el.textContent : ''; \
                      const start = Math.min(arguments[1], text.length); \
                      return { chunk: text.slice(start), next: text.length };";
        let path = format!("/session/{}/execute/sync", self.id);
        let reply = self
            .call(
                reqwest::Method::POST,
                &path,
                Some(json!({ "script": script, "args": [id, offset] })),
            )
            .await?;
        let chunk = reply
            .pointer("/value/chunk")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let next = reply
            .pointer("/value/next")
            .and_then(Value::as_u64)
            .map_or(offset, |next| usize::try_from(next).unwrap_or(offset));
        Ok((chunk.to_owned(), next))
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("WebDriver {path}"))?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            bail!("WebDriver {path} answered {status}: {text}");
        }
        let reply: Value = serde_json::from_str(&text)
            .with_context(|| format!("WebDriver {path} answered {text}"))?;
        // The legacy protocol Chrome is driven with reports failures in a
        // non-zero `status` under a success HTTP code.
        if reply
            .get("status")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
        {
            bail!("WebDriver {path} answered {text}");
        }
        Ok(reply)
    }

    pub(crate) async fn close(mut self) {
        let path = format!("/session/{}", self.id);
        match timeout(
            SESSION_CLOSE,
            self.call(reqwest::Method::DELETE, &path, None),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => eprintln!("closing the browser session failed: {error:#}"),
            Err(_) => {
                eprintln!("the driver did not close the browser session within {SESSION_CLOSE:?}");
            }
        }
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.driver.kill();
        let _ = self.driver.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Chrome is opened with the legacy `desiredCapabilities` body that
/// `webdriver.json`'s `w3c: false` asks for; Firefox and Safari speak W3C.
fn new_session(browser: Browser, mut capabilities: Map<String, Value>) -> Value {
    let mut headless = |options: &str, arguments: &[&str]| {
        let entry = capabilities.entry(options).or_insert_with(|| json!({}));
        if let Some(options) = entry.as_object_mut() {
            let list = options.entry("args").or_insert_with(|| json!([]));
            if let Some(list) = list.as_array_mut() {
                list.extend(arguments.iter().map(|argument| json!(argument)));
            }
        }
    };
    match browser {
        Browser::Chrome => {
            headless(
                "goog:chromeOptions",
                &["headless", "disable-dev-shm-usage", "no-sandbox"],
            );
            json!({ "desiredCapabilities": capabilities, "requiredCapabilities": {} })
        }
        Browser::Firefox => {
            headless("moz:firefoxOptions", &["-headless"]);
            json!({ "capabilities": { "alwaysMatch": capabilities, "firstMatch": [{}] } })
        }
        Browser::Safari => json!({ "desiredCapabilities": {}, "capabilities": {} }),
    }
}
