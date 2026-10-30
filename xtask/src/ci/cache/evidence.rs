use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use kithara_devtools::common::tools::ToolsConfig;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{ci::process::Process, consts};

/// Clear at lane entry, including runs that fail before reaching cache reporting.
pub(in crate::ci) fn clear(process: &Process, lane: &str) {
    let Ok(path) = report_path(process, lane) else {
        return;
    };
    if let Err(error) = fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warn!("cannot remove stale cache evidence");
    }
}

pub(in crate::ci) fn report_path(process: &Process, lane: &str) -> Result<PathBuf> {
    ensure!(
        !lane.is_empty()
            && lane.len() <= 128
            && lane
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "invalid cache evidence lane"
    );
    let configured = |name: &str| {
        process
            .environment_var(name)
            .filter(|value| !value.is_empty())
    };
    let number = |name: &str| -> Result<u64> {
        let value = configured(name).context("missing cache evidence job identity")?;
        Ok(value
            .to_str()
            .context("invalid cache evidence job identity")?
            .parse()?)
    };
    let key = if configured("CI_JOB_ID").is_some() {
        format!("gitlab-{}-{lane}", number("CI_JOB_ID")?)
    } else if configured("GITHUB_RUN_ID").is_some() {
        format!(
            "github-{}-{}-{lane}",
            number("GITHUB_RUN_ID")?,
            number("GITHUB_RUN_ATTEMPT")?
        )
    } else {
        format!("local-{}-{lane}", std::process::id())
    };
    Ok(process
        .root()
        .join(".ci-artifacts/cache")
        .join(format!("{key}.json")))
}

pub(super) fn record(phase: &str) -> Result<()> {
    let root = std::env::current_dir()?;
    let tools = kithara_devtools::common::project::ProjectConfig::load(&root)?.tools;
    let process = Process::new(&root, std::collections::BTreeMap::new());
    let primary = process.capture_bounded(
        tools.program("kache"),
        &["stats", "--json"],
        "kache statistics",
        consts::CACHE_EVIDENCE_INPUT_LIMIT,
    );
    let fallback = process.capture_bounded(
        tools.program("sccache"),
        &["--show-stats"],
        "sccache statistics",
        consts::CACHE_EVIDENCE_INPUT_LIMIT,
    );
    retain(&process, &tools, &primary, &fallback, phase)
}

/// Retain only typed observations before the caller stops its cache daemon.
pub(in crate::ci) fn retain(
    process: &Process,
    tools: &ToolsConfig,
    primary: &Result<String>,
    fallback: &Result<String>,
    lane: &str,
) -> Result<()> {
    let full = process.capture_bounded(
        tools.program("kache"),
        &["stats", "--full", "--json"],
        "full kache statistics",
        consts::CACHE_EVIDENCE_INPUT_LIMIT,
    );
    let explain = process.capture_bounded(
        tools.program("kache"),
        &["explain", "--json"],
        "kache miss explanations",
        consts::CACHE_EVIDENCE_INPUT_LIMIT,
    );
    let mut report = observations(
        parsed(primary),
        parsed(&full),
        parsed(&explain),
        fallback.as_ref().ok().map(String::as_str),
    );
    let var = |name| {
        process
            .environment_var(name)
            .and_then(|v| v.into_string().ok())
    };
    // Read the checkout, rather than treating a provider's claimed SHA as execution proof.
    let revision = process
        .capture_bounded(
            tools.program("git"),
            &["rev-parse", "HEAD"],
            "cache evidence revision",
            64,
        )
        .ok();
    report["revision"] = json!(revision.as_deref().map(str::trim).and_then(|s| hex(s, 40)));
    report["lane"] = json!(lane);
    report["job"] = json!({
        "gitlab_job_id": var("CI_JOB_ID").and_then(|v| v.parse::<u64>().ok()),
        "github_run_id": var("GITHUB_RUN_ID").and_then(|v| v.parse::<u64>().ok()),
        "github_run_attempt": var("GITHUB_RUN_ATTEMPT").and_then(|v| v.parse::<u64>().ok()),
        "github_job_sha256": var("GITHUB_JOB").filter(|v| !v.is_empty()).map(|v| hex::encode(Sha256::digest(v.as_bytes()))),
    });
    report["steps"] = json!(process.timing_evidence());
    report["key_history"] = history(process).unwrap_or(Value::Null);
    let bytes = serde_json::to_vec(&report)?;
    ensure!(
        bytes.len() <= 256 * 1024,
        "cache evidence exceeds its size bound"
    );
    let path = report_path(process, lane)?;
    fs::create_dir_all(process.root().join(".ci-artifacts/cache"))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(process.root().join(".ci-artifacts/cache"))?;
    std::io::Write::write_all(&mut temporary, &bytes)?;
    temporary
        .persist(path)
        .context("publishing cache evidence")?;
    Ok(())
}

fn parsed(input: &Result<String>) -> Option<Value> {
    let text = input.as_ref().ok()?;
    (text.len() <= consts::CACHE_EVIDENCE_INPUT_LIMIT)
        .then(|| serde_json::from_str(text).ok())
        .flatten()
}

fn numbers(input: &Value, fields: &str) -> Value {
    fields
        .split_whitespace()
        .map(|field| (field.to_owned(), json!(input[field].as_u64())))
        .collect()
}

fn hex(text: &str, len: usize) -> Option<&str> {
    (text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())).then_some(text)
}

pub(in crate::ci) fn sccache_count(stats: &str, label: &str) -> Option<u64> {
    stats.lines().find_map(|line| {
        let (name, value) = line.trim_end().rsplit_once(char::is_whitespace)?;
        (name.trim() == label)
            .then(|| value.parse::<u64>().ok())
            .flatten()
    })
}

fn observations(
    primary: Option<Value>,
    full: Option<Value>,
    explain: Option<Value>,
    fallback: Option<&str>,
) -> Value {
    let primary = primary.unwrap_or(Value::Null);
    let full = full.unwrap_or(Value::Null);
    let explain = explain.unwrap_or(Value::Null);
    let causes = explain["causes"].as_array().map(|causes| {
        causes
            .iter()
            .take(consts::CACHE_EVIDENCE_RECORD_LIMIT)
            .map(|cause| {
                let category = cause["why"].as_str().and_then(|why| match why {
                    "built before with this exact key, but the entry was gone" => {
                        Some("entry_absent")
                    }
                    "no earlier compile in the loaded history" => Some("no_loaded_history"),
                    "unexplained (no key diff recorded)" => Some("unexplained"),
                    _ if why.starts_with("compiled but not cached: ") => Some("store_failed"),
                    _ if why.starts_with("entry rejected at lookup: ") => Some("lookup_rejected"),
                    _ if why.starts_with("own inputs changed: ") => Some("inputs_changed"),
                    _ if why.starts_with("downstream of ") => Some("dependency_changed"),
                    _ if why.starts_with("differs from the build in ") => Some("other_checkout"),
                    _ => None,
                });
                let mut safe = numbers(cause, "count compile_ms");
                safe["category"] = json!(category);
                safe["failure"] = json!(cause["failure"].as_bool());
                safe
            })
            .collect::<Vec<_>>()
    });
    let fallback_count = |label| fallback.and_then(|text| sccache_count(text, label));
    json!({
        "schema_version": 1,
        "kache": {
            "schema_version": primary["schema_version"].as_u64(),
            "counts": numbers(&primary, "local_hits prefetch_hits remote_hits dups misses total_elapsed_ms hit_elapsed_ms miss_elapsed_ms time_saved_ms"),
            "daemon_connected": primary["daemon_connected"].as_bool(),
            "remote_s3": primary["remote"].as_str().map(|remote| remote.starts_with("s3://")),
            "remote_sha256": primary["remote"].as_str().map(|remote| hex::encode(Sha256::digest(remote.as_bytes()))),
            "daemon_remote_readonly": primary["daemon_remote_readonly"].as_bool(),
            "disk": numbers(&primary["disk"], "store_bytes store_limit_bytes disk_private_bytes cloned_into_targets_bytes snapshot_retained_bytes"),
            "gc": numbers(&primary["gc"], "entries_evicted bytes_freed duration_ms entries_pinned entries_unreclaimable bytes_held entries_failed entries_locked entries_busy_snapshot entries_recent_prefiltered entries_import_pinned"),
            "gc_scope": "latest_store_observation",
            "full_schema_version": full["schema_version"].as_u64(),
            "summary": numbers(&full["summary"], "local_hits prefetch_hits remote_hits misses errors store_failures fallbacks total_duration_ms"),
            "timing": numbers(&full["timing"], "hit_time_ms miss_time_ms miss_compile_time_ms total_key_ms total_lookup_ms total_restore_ms total_store_ms total_startup_ms total_dep_info_ms total_wait_ms total_flight_wait_ms total_permit_wait_ms total_unattributed_ms"),
            "network": numbers(&full["network"], "bytes_up bytes_down uploads_ok uploads_failed downloads_ok downloads_failed total_request_ms total_body_ms total_semaphore_wait_ms total_import_lock_wait_ms"),
            "storage": numbers(&full["storage"], "store_entries store_blobs blob_bytes logical_bytes restored_bytes copied_bytes hardlinked_bytes reflinked_bytes"),
            "top_misses": costly_units(&full["top_misses"]),
            "top_hits": costly_units(&full["top_hits"]),
            "explain": {"scope": "job_runtime_only", "prior_jobs": null, "counts": numbers(&explain, "misses earlier_misses unexplained"), "causes": causes},
        },
        "sccache": {
            "hits": fallback_count("Cache hits"), "misses": fallback_count("Cache misses"),
            "write_errors": fallback_count("Cache write errors"), "read_errors": fallback_count("Cache read errors"),
            "compilations": fallback_count("Compilations"), "failures": fallback_count("Compilation failures"),
        },
        "queue_ms": {"dependency": null, "resource": null, "runner": null},
        "actual_linker": null,
    })
}

fn costly_units(input: &Value) -> Value {
    json!(input.as_array().map(|units| {
        units
            .iter()
            .take(16)
            .map(|unit| {
                let mut safe = numbers(
                    unit,
                    "elapsed_ms compile_time_ms overhead_ms size compiler_runs",
                );
                safe["cache_key"] = json!(unit["cache_key"].as_str().and_then(|key| hex(key, 64)));
                safe["crate_name"] = json!(unit["crate_name"].as_str().filter(|name| {
                    !name.is_empty()
                        && name.len() <= 128
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                }));
                safe
            })
            .collect::<Vec<_>>()
    }))
}

fn history(process: &Process) -> Option<Value> {
    let config = fs::read_to_string(process.environment_path("KACHE_CONFIG")?).ok()?;
    let config: toml::Value = toml::from_str(&config).ok()?;
    let runtime = config.get("cache")?.get("runtime_dir")?.as_str()?;
    let file = fs::File::open(Path::new(runtime).join("events.jsonl")).ok()?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(consts::CACHE_EVIDENCE_INPUT_LIMIT + 1).ok()?)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() > consts::CACHE_EVIDENCE_INPUT_LIMIT;
    bytes.truncate(consts::CACHE_EVIDENCE_INPUT_LIMIT);
    let events: Vec<Value> = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    let invalid_records = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count()
        .saturating_sub(events.len());
    let records: Vec<_> = events
        .iter()
        .take(consts::CACHE_EVIDENCE_RECORD_LIMIT)
        .map(|event| {
            let mut safe = numbers(
                event,
                "schema elapsed_ms compile_time_ms key_ms lookup_ms restore_ms store_ms",
            );
            for (field, len) in [("cache_key", 64), ("unit_id", 16)] {
                safe[field] = json!(event[field].as_str().and_then(|s| hex(s, len)));
            }
            safe["key_externs_recorded"] = json!(event["key_externs_recorded"].as_bool());
            safe["key_diff"] = json!(event["key_diff"].as_array().map(|groups| {
                groups
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|group| {
                        consts::CACHE_EVIDENCE_KEY_GROUPS
                            .split_whitespace()
                            .any(|known| known == *group)
                    })
                    .take(9)
                    .collect::<Vec<_>>()
            }));
            safe["result"] = json!(event["result"].as_str().filter(|v| matches!(
                *v,
                "local_hit"
                    | "prefetch_hit"
                    | "remote_hit"
                    | "dup"
                    | "miss"
                    | "error"
                    | "passthrough"
                    | "skipped"
            )));
            safe["key_fields"] = consts::CACHE_EVIDENCE_KEY_GROUPS
                .split_whitespace()
                .map(|group| {
                    (
                        group.to_owned(),
                        json!(event["key_fields"][group].as_str().and_then(|s| hex(s, 16))),
                    )
                })
                .collect();
            safe
        })
        .collect();
    Some(
        json!({"scope": "job_runtime_only", "prior_jobs": null, "invalid_records": invalid_records, "truncated": truncated || events.len() > consts::CACHE_EVIDENCE_RECORD_LIMIT || invalid_records > 0, "records": records}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleared_ci_identities_select_local_evidence() {
        let root = tempfile::tempdir().unwrap();
        let process = Process::new(
            root.path(),
            std::collections::BTreeMap::from([
                ("CI_JOB_ID".into(), "".into()),
                ("GITHUB_RUN_ID".into(), "".into()),
            ]),
        );
        assert_eq!(
            report_path(&process, "test").unwrap(),
            root.path().join(format!(
                ".ci-artifacts/cache/local-{}-test.json",
                std::process::id()
            ))
        );
    }

    #[test]
    fn cache_evidence_drops_secrets_even_in_known_fields() {
        let secret = "AWS_SECRET_ACCESS_KEY=synthetic-secret";
        let input = json!({"schema_version": 1, "local_hits": 4, "misses": secret,
            "remote": secret, "disk": {"store_bytes": secret},
            "summary": {"errors": 2, "store_failures": secret},
            "causes": [{"why": format!("compiled but not cached: {secret}"), "fix": secret, "count": 3, "compile_ms": secret, "failure": true}],
            "errors_detail": [{"message": secret}], "env": {"AWS_SECRET_ACCESS_KEY": secret}});
        let report = observations(
            Some(input.clone()),
            Some(input.clone()),
            Some(input),
            Some("Cache hits 9\nCache misses 8\nCache write errors secret\n"),
        );
        assert!(!report.to_string().contains(secret));
        assert_eq!(report["kache"]["counts"]["local_hits"], 4);
        assert_eq!(report["sccache"]["hits"], 9);
        assert_eq!(report["sccache"]["misses"], 8);
        assert_eq!(
            report["kache"]["explain"]["causes"][0]["category"],
            "store_failed"
        );
        assert!(report["kache"]["counts"]["misses"].is_null());
        assert!(report["kache"]["disk"]["store_bytes"].is_null());
        assert!(report["sccache"]["write_errors"].is_null());
    }

    #[test]
    fn unavailable_cache_observations_stay_unknown() {
        let report = observations(None, None, None, None);
        assert!(report["kache"]["counts"]["misses"].is_null());
        assert!(report["kache"]["explain"]["counts"]["earlier_misses"].is_null());
        assert!(report["kache"]["gc"]["bytes_held"].is_null());
        assert!(report["actual_linker"].is_null());
        assert!(report["queue_ms"]["runner"].is_null());
        assert!(parsed(&Ok("x".repeat(consts::CACHE_EVIDENCE_INPUT_LIMIT + 1))).is_none());
    }

    #[test]
    fn costly_units_keep_measured_costs_without_compiler_inputs() {
        let input = json!({"top_misses": [{"crate_name": "kithara_dsp_tests", "cache_key": "a".repeat(64),
            "compile_time_ms": 120000, "elapsed_ms": 120500, "command": "AWS_SECRET_ACCESS_KEY=secret"},
            {"crate_name": "AWS_SECRET_ACCESS_KEY=secret", "compile_time_ms": "secret"}]});
        let report = observations(None, Some(input), None, None);
        assert_eq!(report["kache"]["top_misses"][0]["compile_time_ms"], 120000);
        assert_eq!(
            report["kache"]["top_misses"][0]["crate_name"],
            "kithara_dsp_tests"
        );
        assert!(report["kache"]["top_misses"][1]["crate_name"].is_null());
        assert!(!report.to_string().contains("secret"));
    }

    #[test]
    fn key_history_survives_runtime_cleanup_without_raw_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let job =
            crate::ci::kache::KacheJob::prepare(directory.path().join("kache"), None).unwrap();
        let tools = ToolsConfig::default();
        let bin = directory.path().join("bin");
        crate::testing::install_double(&bin, "kache");
        crate::testing::install_double(&bin, "git");
        let mut vars: std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString> = job
            .environment(&tools)
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect();
        vars.insert("PATH".into(), bin.into());
        vars.insert("CI_JOB_ID".into(), "123".into());
        vars.insert("GITHUB_JOB".into(), "AWS_SECRET_ACCESS_KEY=secret".into());
        vars.insert(
            "GITHUB_RUN_ID".into(),
            "AWS_SECRET_ACCESS_KEY=secret".into(),
        );
        vars.insert("KITHARA_TEST_STDOUT".into(), "a".repeat(40).into());
        let process = Process::new(directory.path(), vars);
        assert!(
            process
                .capture_bounded(tools.program("kache"), &["stats"], "bounded report", 1)
                .is_err()
        );
        let event = json!({"cache_key": "a".repeat(64), "unit_id": "b".repeat(16), "schema": 20,
            "key_fields": {"compiler": "c".repeat(16), "args": "AWS_SECRET_ACCESS_KEY=secret", "AWS_SECRET_ACCESS_KEY": "secret"},
            "key_env_deps": {"AWS_SECRET_ACCESS_KEY": "secret"}, "command": "secret"});
        fs::write(
            directory.path().join("kache/runtime/events.jsonl"),
            format!(
                "{}invalid\n",
                format!("{event}\n").repeat(consts::CACHE_EVIDENCE_RECORD_LIMIT + 1)
            ),
        )
        .unwrap();
        let retained = history(&process).unwrap();
        assert_eq!(
            retained["records"].as_array().unwrap().len(),
            consts::CACHE_EVIDENCE_RECORD_LIMIT
        );
        assert_eq!(retained["truncated"], true);
        assert_eq!(retained["invalid_records"], 1);
        assert!(retained["prior_jobs"].is_null());
        assert_eq!(
            retained["records"][0]["key_fields"]["compiler"],
            "c".repeat(16)
        );
        assert!(!retained.to_string().contains("secret"));
        retain(
            &process,
            &tools,
            &Ok("{}".into()),
            &Ok(String::new()),
            "test",
        )
        .unwrap();
        let path = report_path(&process, "test").unwrap();
        let report: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(report["key_history"], retained);
        assert_eq!(report["job"]["gitlab_job_id"], 123);
        assert_eq!(report["revision"], "a".repeat(40));
        assert!(report["job"]["github_run_id"].is_null());
        assert!(!report.to_string().contains("secret"));
        job.stop(&process, &tools);
        drop(job);
        assert!(!directory.path().join("kache/runtime").exists());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
            report
        );
        clear(&process, "test");
        assert!(!path.exists());
    }

    #[test]
    fn inherited_producer_reports_are_not_a_consumers_upload_target() {
        let root = tempfile::tempdir().unwrap();
        let producer = Process::new(
            root.path(),
            std::collections::BTreeMap::from([("CI_JOB_ID".into(), "1".into())]),
        );
        let consumer = Process::new(
            root.path(),
            std::collections::BTreeMap::from([("CI_JOB_ID".into(), "2".into())]),
        );
        let producer_path = report_path(&producer, "apple-lint").unwrap();
        fs::create_dir_all(producer_path.parent().unwrap()).unwrap();
        fs::write(&producer_path, "upstream report").unwrap();
        let consumer_path = report_path(&consumer, "apple-test").unwrap();
        assert_eq!(
            consumer_path.file_name().unwrap(),
            "gitlab-2-apple-test.json"
        );
        assert_ne!(producer_path, consumer_path);
        assert!(
            !consumer_path.exists(),
            "bootstrap failure must leave no current-job report to upload"
        );
        assert!(producer_path.exists());
    }
}
