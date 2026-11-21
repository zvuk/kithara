//! Browser capabilities for the `WebDriver` session, from `webdriver.json`.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};

use crate::driver::Browser;

/// The directory the CI lane that owns it names for Chrome network logs;
/// without it no capture is chosen.
const NET_LOG_DIR: &str = "KITHARA_WEB_NETLOG_DIR";

// Limit network traces to the five known stall fixtures: the worklet-import pair, the observer suite and both lib-test binaries.
const TRACED_FIXTURES: [&str; 5] = [
    "web_bootstrap-",
    "web_analysis-",
    "kithara_ffi-",
    "kithara_host-",
    "web_observer-",
];

/// `webdriver.json` with Chrome's network and page tracing added for the binaries
/// whose startup stalls are being traced, and their network log when the lane
/// names a directory for it.
pub(crate) fn load(browser: Browser, wasm: &Path) -> Result<Map<String, Value>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("webdriver.json");
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut config: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if browser == Browser::Chrome
        && let Some(stem) = perf_logging_target(wasm)
    {
        apply_perf_logging(&mut config).map_err(|error| anyhow!(error))?;
        eprintln!("webdriver config: fixture network perf logging requested");
        if let Some(dir) = env::var_os(NET_LOG_DIR) {
            let file = net_log_file(&mut config, Path::new(&dir), stem)?;
            eprintln!(
                "webdriver config: fixture NetLog capture requested at {}",
                file.display()
            );
        }
    }
    match config {
        Value::Object(map) => Ok(map),
        _ => Err(anyhow!("{} is not a JSON object", path.display())),
    }
}

/// The wasm file stem when the run targets one of the traced fixtures.
fn perf_logging_target(wasm: &Path) -> Option<&str> {
    let stem = wasm.file_name()?.to_str()?.strip_suffix(".wasm")?;
    TRACED_FIXTURES
        .iter()
        .any(|prefix| stem.starts_with(prefix))
        .then_some(stem)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum NetLogError {
    #[error("{0} is not a JSON {1}")]
    Shape(&'static str, &'static str),
    #[error("webdriver.json already carries the NetLog switch `{0}`")]
    Conflict(String),
    #[error("NetLog directory is not an absolute UTF-8 path: {0}")]
    Dir(PathBuf),
    #[error("NetLog path {path} is not usable: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Names this invocation's Chrome network log in the CI-owned directory and adds
/// its switches to `config`. Only once the configuration is accepted is a file a
/// previous run of the same binary left there removed, so a file present after
/// the run was written by this run's browser.
pub(crate) fn net_log_file(
    config: &mut Value,
    dir: &Path,
    stem: &str,
) -> Result<PathBuf, NetLogError> {
    if !dir.is_absolute() || dir.to_str().is_none() {
        return Err(NetLogError::Dir(dir.to_path_buf()));
    }
    let unusable = |path: &Path, source| NetLogError::Io {
        path: path.to_path_buf(),
        source,
    };
    let metadata = fs::metadata(dir).map_err(|source| unusable(dir, source))?;
    if !metadata.is_dir() {
        return Err(unusable(dir, io::ErrorKind::NotADirectory.into()));
    }
    let file = dir.join(format!("{stem}.jsonl"));
    apply_net_log(config, &file)?;
    match fs::remove_file(&file) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => Err(unusable(&file, source)),
        _ => Ok(file),
    }
}

/// Adds the default-capture network-log switches for `file` to the Chrome
/// arguments. Any other network-log switch already present is refused rather than kept, so a
/// sensitive capture mode or a second log path can never ride along.
pub(crate) fn apply_net_log(config: &mut Value, file: &Path) -> Result<(), NetLogError> {
    let file = file
        .to_str()
        .ok_or_else(|| NetLogError::Dir(file.to_path_buf()))?;
    let switches = [
        format!("--log-net-log={file}"),
        "--net-log-capture-mode=Default".to_string(),
        "--net-log-file-format=ndjson".to_string(),
    ];
    let args = config
        .as_object_mut()
        .ok_or(NetLogError::Shape("webdriver.json", "object"))?
        .entry("goog:chromeOptions".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(NetLogError::Shape("goog:chromeOptions", "object"))?
        .entry("args".to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or(NetLogError::Shape("goog:chromeOptions.args", "array"))?;
    for arg in args.iter() {
        let arg = arg
            .as_str()
            .ok_or(NetLogError::Shape("goog:chromeOptions.args item", "string"))?;
        let name = arg.trim_start_matches('-');
        let net_log = name.starts_with("log-net-log") || name.starts_with("net-log-");
        if net_log && !switches.iter().any(|switch| switch == arg) {
            return Err(NetLogError::Conflict(arg.to_string()));
        }
    }
    for switch in switches {
        if !args.iter().any(|arg| arg.as_str() == Some(switch.as_str())) {
            args.push(Value::String(switch));
        }
    }
    Ok(())
}

fn apply_perf_logging(config: &mut Value) -> Result<(), String> {
    let root = config
        .as_object_mut()
        .ok_or_else(|| "webdriver.json is not a JSON object".to_string())?;
    let logging = root
        .entry("goog:loggingPrefs".to_string())
        .or_insert_with(|| json!({}));
    logging
        .as_object_mut()
        .ok_or_else(|| "goog:loggingPrefs is not a JSON object".to_string())?
        .entry("performance".to_string())
        .or_insert_with(|| json!("ALL"));
    let chrome_options = root
        .entry("goog:chromeOptions".to_string())
        .or_insert_with(|| json!({}));
    let perf = chrome_options
        .as_object_mut()
        .ok_or_else(|| "goog:chromeOptions is not a JSON object".to_string())?
        .entry("perfLoggingPrefs".to_string())
        .or_insert_with(|| json!({}));
    let perf = perf
        .as_object_mut()
        .ok_or_else(|| "perfLoggingPrefs is not a JSON object".to_string())?;
    perf.entry("enableNetwork".to_string())
        .or_insert_with(|| json!(true));
    perf.entry("enablePage".to_string())
        .or_insert_with(|| json!(true));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use kithara_test_utils::kithara;
    use serde_json::json;

    use super::{
        NetLogError, apply_net_log, apply_perf_logging, net_log_file, perf_logging_target,
    };

    const NET_LOG_SWITCHES: [&str; 2] = [
        "--net-log-capture-mode=Default",
        "--net-log-file-format=ndjson",
    ];

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
            "/cache/deps/web_observer-894faf148f0e514e.wasm",
        ] {
            let stem = Path::new(binary).file_stem().and_then(|stem| stem.to_str());
            assert_eq!(perf_logging_target(Path::new(binary)), stem);
        }
        let other = "/cache/deps/kithara_platform-5f7e0acd8e3ff6b5.wasm";
        assert_eq!(perf_logging_target(Path::new(other)), None);
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

    #[kithara::test(native)]
    fn net_log_adds_exactly_the_default_capture_switches_and_keeps_every_prior_key() {
        let mut config = config();
        let file = Path::new("/ci/netlog/web_bootstrap-8f845460d7dd326c.jsonl");
        apply_net_log(&mut config, file).expect("merge applies");
        let chrome = &config["goog:chromeOptions"];
        assert_eq!(
            chrome["args"],
            json!([
                "--headless=new",
                "--log-net-log=/ci/netlog/web_bootstrap-8f845460d7dd326c.jsonl",
                NET_LOG_SWITCHES[0],
                NET_LOG_SWITCHES[1],
            ])
        );
        assert_eq!(chrome["w3c"], json!(false));
        assert_eq!(
            chrome["perfLoggingPrefs"],
            json!({ "enableTimeline": false })
        );
        assert_eq!(config["moz:firefoxOptions"]["args"], json!(["-headless"]));
        assert_eq!(config["custom:capability"], json!({ "kept": true }));

        let once = config.clone();
        apply_net_log(&mut config, file).expect("second merge");
        assert_eq!(config, once);
    }

    #[kithara::test(native)]
    #[case::sensitive_mode("--net-log-capture-mode=IncludeSensitive")]
    #[case::socket_bytes("--net-log-capture-mode=Everything")]
    #[case::size_cap("--net-log-max-size-mb=10")]
    #[case::duration("--net-log-duration=5")]
    #[case::second_path("--log-net-log=/elsewhere/netlog.json")]
    #[case::single_dash("-log-net-log=/elsewhere/netlog.json")]
    #[case::json_format("--net-log-file-format=json")]
    fn net_log_refuses_a_conflicting_switch(#[case] present: &str) {
        let mut config = json!({ "goog:chromeOptions": { "args": ["--headless=new", present] } });
        let file = Path::new("/ci/netlog/web_observer-894faf148f0e514e.jsonl");
        let error = apply_net_log(&mut config, file).expect_err("conflict is refused");
        assert!(
            matches!(&error, NetLogError::Conflict(arg) if arg == present),
            "{error}"
        );
    }

    #[kithara::test(native)]
    fn net_log_wrong_shapes_and_paths_are_errors_not_panics() {
        let file = Path::new("/ci/netlog/kithara_ffi-ec784db4d0559dc9.jsonl");
        for mut bad in [
            json!([]),
            json!({ "goog:chromeOptions": 1 }),
            json!({ "goog:chromeOptions": { "args": 1 } }),
            json!({ "goog:chromeOptions": { "args": [1] } }),
        ] {
            assert!(matches!(
                apply_net_log(&mut bad, file),
                Err(NetLogError::Shape(..))
            ));
        }
        for dir in ["", "relative/netlog"] {
            assert!(matches!(
                net_log_file(
                    &mut config(),
                    Path::new(dir),
                    "web_analysis-3b2b9b5370fa3bf7"
                ),
                Err(NetLogError::Dir(_))
            ));
        }
    }

    #[kithara::test(native)]
    fn net_log_file_is_fresh_for_this_invocation_and_touches_only_its_own_leaf() {
        let dir = tempfile::tempdir().expect("temp dir");
        let stem = "kithara_host-260fbe53033cc5d3";
        let own = dir.path().join(format!("{stem}.jsonl"));
        let foreign = dir.path().join("web_observer-894faf148f0e514e.jsonl");
        fs::write(&own, "stale").expect("stale own leaf");
        fs::write(&foreign, "other binary").expect("foreign leaf");

        assert_eq!(
            net_log_file(&mut config(), dir.path(), stem).expect("usable dir"),
            own
        );
        assert!(
            !own.exists(),
            "a stale same-stem file is never kept as current"
        );
        assert_eq!(fs::read_to_string(&foreign).expect("kept"), "other binary");

        let missing = dir.path().join("missing");
        assert!(matches!(
            net_log_file(&mut config(), &missing, stem),
            Err(NetLogError::Io { .. })
        ));
        assert!(!missing.exists(), "the directory belongs to the CI owner");
    }

    #[kithara::test(native)]
    fn net_log_rejected_config_keeps_the_existing_leaf() {
        let dir = tempfile::tempdir().expect("temp dir");
        let stem = "web_bootstrap-8f845460d7dd326c";
        let own = dir.path().join(format!("{stem}.jsonl"));
        fs::write(&own, "previous run").expect("existing own leaf");
        let mut conflicting = json!({
            "goog:chromeOptions": { "args": ["--net-log-capture-mode=IncludeSensitive"] }
        });

        assert!(matches!(
            net_log_file(&mut conflicting, dir.path(), stem),
            Err(NetLogError::Conflict(_))
        ));
        assert_eq!(
            fs::read_to_string(&own).expect("leaf kept"),
            "previous run",
            "a refused configuration aborts before the leaf is touched"
        );
    }
}
