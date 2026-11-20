//! Browser capabilities for the `WebDriver` session, from `webdriver.json`.

use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value};

use crate::driver::Browser;

/// `webdriver.json` with Chrome's network and page tracing added for the binaries
/// whose startup stalls are being traced.
pub(crate) fn load(browser: Browser, wasm: &Path) -> Result<Map<String, Value>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("webdriver.json");
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut config: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if browser == Browser::Chrome && perf_logging_target(wasm) {
        apply_perf_logging(&mut config).map_err(|error| anyhow!(error))?;
        eprintln!("webdriver config: fixture network perf logging requested");
    }
    match config {
        Value::Object(map) => Ok(map),
        _ => Err(anyhow!("{} is not a JSON object", path.display())),
    }
}

// Limit network traces to the four known stall fixtures: the worklet-import pair and both lib-test binaries.
fn perf_logging_target(wasm: &Path) -> bool {
    let name = wasm.file_name().map(|name| name.to_string_lossy());
    matches!(
        name.as_deref(),
        Some(name)
            if (name.starts_with("web_bootstrap-")
                || name.starts_with("web_analysis-")
                || name.starts_with("kithara_ffi-")
                || name.starts_with("kithara_host-"))
                && name.ends_with(".wasm")
    )
}

fn apply_perf_logging(config: &mut Value) -> Result<(), String> {
    let root = config
        .as_object_mut()
        .ok_or_else(|| "webdriver.json is not a JSON object".to_string())?;
    let logging = root
        .entry("goog:loggingPrefs".to_string())
        .or_insert_with(|| serde_json::json!({}));
    logging
        .as_object_mut()
        .ok_or_else(|| "goog:loggingPrefs is not a JSON object".to_string())?
        .entry("performance".to_string())
        .or_insert_with(|| serde_json::json!("ALL"));
    let chrome_options = root
        .entry("goog:chromeOptions".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let perf = chrome_options
        .as_object_mut()
        .ok_or_else(|| "goog:chromeOptions is not a JSON object".to_string())?
        .entry("perfLoggingPrefs".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let perf = perf
        .as_object_mut()
        .ok_or_else(|| "perfLoggingPrefs is not a JSON object".to_string())?;
    perf.entry("enableNetwork".to_string())
        .or_insert_with(|| serde_json::json!(true));
    perf.entry("enablePage".to_string())
        .or_insert_with(|| serde_json::json!(true));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kithara_test_utils::kithara;
    use serde_json::json;

    use super::{apply_perf_logging, perf_logging_target};

    fn config() -> serde_json::Value {
        json!({
            "goog:chromeOptions": {
                "args": ["--headless=new"],
                "w3c": false,
                "perfLoggingPrefs": { "enableTimeline": false }
            },
            "goog:loggingPrefs": { "browser": "INFO" },
            "moz:firefoxOptions": { "args": ["-headless"] },
            "custom:capability": { "kept": true }
        })
    }

    #[kithara::test(native)]
    fn chrome_fixture_targets_are_scoped() {
        for binary in [
            "/cache/deps/web_bootstrap-8f845460d7dd326c.wasm",
            "/cache/deps/web_analysis-3b2b9b5370fa3bf7.wasm",
            "/cache/deps/kithara_ffi-ec784db4d0559dc9.wasm",
            "/cache/deps/kithara_host-260fbe53033cc5d3.wasm",
        ] {
            assert!(perf_logging_target(Path::new(binary)));
        }
        let other = "/cache/deps/web_observer-fc399469e0e06114.wasm";
        assert!(!perf_logging_target(Path::new(other)));
    }

    #[kithara::test(native)]
    fn merge_enables_network_and_keeps_every_prior_key() {
        let mut config = config();
        apply_perf_logging(&mut config).expect("merge applies");
        assert_eq!(config["goog:loggingPrefs"]["performance"], json!("ALL"));
        assert_eq!(config["goog:loggingPrefs"]["browser"], json!("INFO"));
        let chrome = &config["goog:chromeOptions"];
        assert_eq!(chrome["perfLoggingPrefs"]["enableNetwork"], json!(true));
        assert_eq!(chrome["perfLoggingPrefs"]["enablePage"], json!(true));
        assert_eq!(chrome["perfLoggingPrefs"]["enableTimeline"], json!(false));
        assert_eq!(chrome["args"], json!(["--headless=new"]));
        assert_eq!(chrome["w3c"], json!(false));
        assert_eq!(config["moz:firefoxOptions"]["args"], json!(["-headless"]));
        assert_eq!(config["custom:capability"], json!({ "kept": true }));
    }

    #[kithara::test(native)]
    fn merge_is_idempotent() {
        let mut config = config();
        apply_perf_logging(&mut config).expect("first merge");
        let once = config.clone();
        apply_perf_logging(&mut config).expect("second merge");
        assert_eq!(config, once);
    }

    #[kithara::test(native)]
    fn wrong_shapes_are_errors_not_panics() {
        for mut bad in [
            json!([]),
            json!({ "goog:loggingPrefs": 1 }),
            json!({ "goog:chromeOptions": { "perfLoggingPrefs": 1 } }),
        ] {
            assert!(apply_perf_logging(&mut bad).is_err());
        }
    }
}
