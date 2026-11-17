use js_sys::{Promise, futures::JsFuture};
use kithara_platform::time::{Duration, timeout};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

#[wasm_bindgen(module = "/src/test/browser_output.js")]
extern "C" {
    #[wasm_bindgen(js_name = OutputStartupProbe)]
    #[derive(Debug)]
    type JsOutputStartupProbe;

    #[wasm_bindgen(constructor, catch, js_class = OutputStartupProbe)]
    fn new() -> Result<JsOutputStartupProbe, JsValue>;

    #[wasm_bindgen(method)]
    fn snapshot(this: &JsOutputStartupProbe) -> Promise;

    #[wasm_bindgen(method, catch)]
    fn restore(this: &JsOutputStartupProbe) -> Result<(), JsValue>;
}

/// Observes real browser output startup while owning its temporary browser hooks.
#[derive(Debug)]
pub struct OutputStartupProbe {
    inner: JsOutputStartupProbe,
}

impl OutputStartupProbe {
    /// Installs observations before constructing an output on the browser main thread.
    ///
    /// # Errors
    /// Returns the browser error if hook installation fails.
    pub fn new() -> Result<Self, JsValue> {
        kithara_platform::thread::assert_main_thread("OutputStartupProbe::new");
        JsOutputStartupProbe::new().map(|inner| Self { inner })
    }

    /// Collects a snapshot within 100ms and restores hooks even when collection fails.
    ///
    /// # Errors
    /// Returns the browser error or an elapsed collection deadline.
    pub async fn snapshot(&self) -> Result<JsValue, JsValue> {
        let result = timeout(
            Duration::from_millis(100),
            JsFuture::from(self.inner.snapshot()),
        )
        .await;
        self.inner.restore()?;
        result.map_err(|_| JsValue::from_str("browser output snapshot timed out"))?
    }
}

impl Drop for OutputStartupProbe {
    fn drop(&mut self) {
        if let Err(error) = self.inner.restore() {
            tracing::error!(?error, "browser output probe restoration failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use js_sys::Reflect;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    #[wasm_bindgen_test]
    async fn a_snapshot_restores_browser_hooks() -> Result<(), JsValue> {
        let global = js_sys::global();
        let property =
            |object: &JsValue, name: &str| Reflect::get(object, &JsValue::from_str(name));
        let locks = property(&property(&global, "navigator")?, "locks")?;
        let audio = property(&global, "AudioContext")?;
        let node = property(&global, "AudioWorkletNode")?;
        let request = property(&locks, "request")?;
        let probe = OutputStartupProbe::new()?;
        assert_ne!(property(&global, "AudioContext")?, audio);
        assert_ne!(property(&locks, "request")?, request);
        // Restoration is required even when the bounded query cannot complete.
        drop(probe.snapshot().await);
        assert_eq!(property(&global, "AudioContext")?, audio);
        assert_eq!(property(&global, "AudioWorkletNode")?, node);
        assert_eq!(property(&locks, "request")?, request);
        drop(probe);
        assert_eq!(property(&locks, "request")?, request);
        Ok(())
    }
}
