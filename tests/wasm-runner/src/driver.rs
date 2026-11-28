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
        let variable = format!("{}_ARGS", browser.variable());
        let arguments = driver_arguments(&env::var(&variable).unwrap_or_default())
            .with_context(|| format!("{variable} does not split as a shell splits it"))?;
        let driver = Command::new(&path)
            .args(arguments)
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

    pub(crate) async fn tests_ready(&self) -> Result<bool> {
        let path = format!("/session/{}/execute/sync", self.id);
        let reply = self
            .call(
                reqwest::Method::POST,
                &path,
                Some(json!({
                    "script": "return window.__kithara_tests_ready === true;",
                    "args": []
                })),
            )
            .await?;
        reply
            .get("value")
            .and_then(Value::as_bool)
            .context("WebDriver returned a non-boolean test readiness value")
    }

    pub(crate) async fn click(&self, id: &str) -> Result<()> {
        let path = format!("/session/{}/element", self.id);
        let reply = self
            .call(
                reqwest::Method::POST,
                &path,
                Some(json!({ "using": "css selector", "value": format!("#{id}") })),
            )
            .await?;
        let element = reply
            .pointer("/value/element-6066-11e4-a52e-4f735466cecf")
            .or_else(|| reply.pointer("/value/ELEMENT"))
            .and_then(Value::as_str)
            .context("WebDriver returned no element id")?;
        let path = format!("/session/{}/element/{element}/click", self.id);
        self.call(reqwest::Method::POST, &path, Some(json!({})))
            .await?;
        Ok(())
    }

    /// The text of element `id` past `offset`, and the offset after it.
    pub(crate) async fn text_since(
        &self,
        id: &str,
        offset: usize,
        deadline: Instant,
    ) -> Result<(String, usize)> {
        let script = "const el = document.getElementById(arguments[0]); \
                      const text = el ? el.textContent : ''; \
                      const start = Math.min(arguments[1], text.length); \
                      return { chunk: text.slice(start), next: text.length };";
        let path = format!("/session/{}/execute/sync", self.id);
        let reply = timeout(
            deadline.saturating_duration_since(Instant::now()),
            self.call(
                reqwest::Method::POST,
                &path,
                Some(json!({ "script": script, "args": [id, offset] })),
            ),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!("reading #{id} through WebDriver {path} exceeded the report deadline")
        })??;
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

/// The extra driver arguments a `<VAR>_ARGS` value holds, split as a shell
/// splits them, so a quoted path with spaces stays one argument.
fn driver_arguments(value: &str) -> Option<Vec<String>> {
    shlex::split(value)
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

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use axum::{Router, routing::post};
    use kithara_test_utils::{TestHttpServer, kithara};

    use super::*;

    #[kithara::test(native)]
    fn driver_arguments_split_as_a_shell_splits() {
        assert_eq!(
            driver_arguments(r"--verbose --log-path='/ci/job'\''s temp/chromedriver.log'"),
            Some(vec![
                "--verbose".to_owned(),
                "--log-path=/ci/job's temp/chromedriver.log".to_owned(),
            ])
        );
        assert_eq!(driver_arguments(""), Some(Vec::new()));
        assert_eq!(driver_arguments("--log-path='unclosed"), None);
    }

    #[kithara::test(native, tokio, timeout(Duration::from_secs(1)))]
    async fn report_deadline_bounds_a_stalled_script_call() {
        let server = TestHttpServer::new(Router::new().route(
            "/session/test/execute/sync",
            post(|| async { std::future::pending::<String>().await }),
        ))
        .await;
        let driver = Command::new(env::current_exe().unwrap())
            .arg("--list")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let session = Session {
            http: reqwest::Client::new(),
            base: server.base_url().as_str().trim_end_matches('/').to_owned(),
            id: "test".into(),
            driver,
        };

        let error = crate::report(&session, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(
                "reading #output through WebDriver /session/test/execute/sync exceeded the report deadline"
            ),
            "{error:#}"
        );
    }
}
