//! Bounded compiler-cache evidence shared by bootstrap and the full CLI.

use std::{
    ffi::OsString,
    fs,
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Maximum bytes read from one cache observation.
pub const INPUT_LIMIT: usize = 8 * 1024 * 1024;

/// Parse one bounded JSON observation; unavailable data remains unknown.
#[must_use]
pub fn parse(input: Option<&str>) -> Option<Value> {
    let text = input?;
    (text.len() <= INPUT_LIMIT)
        .then(|| serde_json::from_str(text).ok())
        .flatten()
}

/// Select evidence by executor and, for bootstrap phases, owned cache runtime.
///
/// # Errors
/// Returns an error for invalid identities or bootstrap phases without a runtime.
pub fn path<F>(root: &Path, lane: &str, environment: F) -> io::Result<PathBuf>
where
    F: Fn(&str) -> Option<OsString>,
{
    if lane.is_empty()
        || lane.len() > 128
        || !lane
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(io::Error::other("invalid cache evidence lane"));
    }
    let configured = |name: &str| environment(name).filter(|v| !v.is_empty());
    let number = |name: &str| -> io::Result<u64> {
        let value = configured(name)
            .ok_or_else(|| io::Error::other("missing cache evidence job identity"))?;
        value
            .to_str()
            .ok_or_else(|| io::Error::other("invalid cache evidence job identity"))?
            .parse()
            .map_err(|_| io::Error::other("invalid cache evidence job identity"))
    };
    let mut key = if configured("CI_JOB_ID").is_some() {
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
    if matches!(lane, "bootstrap" | "bootstrap-driver" | "bootstrap-refresh") {
        let runtime = runtime_sha256(&environment)
            .ok_or_else(|| io::Error::other("missing bootstrap cache runtime"))?;
        key.push('-');
        key.push_str(&runtime);
    }
    Ok(root.join(".ci-artifacts/cache").join(format!("{key}.json")))
}

fn runtime_sha256<F>(environment: F) -> Option<String>
where
    F: Fn(&str) -> Option<OsString>,
{
    environment("KACHE_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(|value| hex::encode(Sha256::digest(value.as_encoded_bytes())))
}

/// Publish the allowlisted observation with sanitized executor metadata.
///
/// # Errors
/// Returns identity, size-bound, serialization or atomic publication errors.
pub fn write<F>(root: &Path, lane: &str, environment: F, mut report: Value) -> io::Result<()>
where
    F: Fn(&str) -> Option<OsString>,
{
    let destination = path(root, lane, &environment)?;
    let var = |name| {
        environment(name)
            .and_then(|v| v.into_string().ok())
            .filter(|v| !v.is_empty())
    };
    report["revision"] = json!(
        report["revision"]
            .as_str()
            .map(str::trim)
            .and_then(|s| hex(s, 40))
    );
    report["lane"] = json!(lane);
    report["job"] = json!({
        "gitlab_job_id": var("CI_JOB_ID").and_then(|v| v.parse::<u64>().ok()),
        "github_run_id": var("GITHUB_RUN_ID").and_then(|v| v.parse::<u64>().ok()),
        "github_run_attempt": var("GITHUB_RUN_ATTEMPT").and_then(|v| v.parse::<u64>().ok()),
        "github_job_sha256": var("GITHUB_JOB").map(|v| hex::encode(Sha256::digest(v.as_bytes()))),
        "cache_runtime_sha256": runtime_sha256(&environment),
    });
    let bytes = serde_json::to_vec(&report)?;
    if bytes.len() > 256 * 1024 {
        return Err(io::Error::other("cache evidence exceeds its size bound"));
    }
    let directory = root.join(".ci-artifacts/cache");
    fs::create_dir_all(&directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    temporary
        .persist(destination)
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(())
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

fn os_error_code(message: &str) -> Option<i32> {
    let (_, code) = message.rsplit_once("(os error ")?;
    code.strip_suffix(')')?.parse().ok()
}

fn store_failure_category(message: &str) -> &'static str {
    if message.ends_with("database is locked") || message.ends_with("database table is locked") {
        "store_sqlite_locked"
    } else {
        "store_failed"
    }
}

/// Read one numeric secondary-cache counter without retaining its text.
#[must_use]
pub fn sccache_count(stats: &str, label: &str) -> Option<u64> {
    stats.lines().find_map(|line| {
        let (name, value) = line.trim_end().rsplit_once(char::is_whitespace)?;
        (name.trim() == label)
            .then(|| value.parse::<u64>().ok())
            .flatten()
    })
}

/// Project provider data onto the existing numeric and digest allowlists.
#[must_use]
pub fn observations(
    primary: Option<Value>,
    full: Option<Value>,
    explain: Option<Value>,
    fallback: Option<&str>,
) -> Value {
    let primary = primary.unwrap_or(Value::Null);
    let full = full.unwrap_or(Value::Null);
    let explain = explain.unwrap_or(Value::Null);
    let causes = explain["causes"].as_array().map(|causes| {
        retained_records(causes)
            .iter()
            .map(|cause| {
                let category = cause["why"].as_str().and_then(|why| match why {
                    "built before with this exact key, but the entry was gone" => {
                        Some("entry_absent")
                    }
                    "no earlier compile in the loaded history" => Some("no_loaded_history"),
                    "unexplained (no key diff recorded)" => Some("unexplained"),
                    _ if why.starts_with("compiled but not cached: ") => {
                        Some(store_failure_category(why))
                    }
                    _ if why.starts_with("entry rejected at lookup: ") => Some("lookup_rejected"),
                    _ if why.starts_with("own inputs changed: ") => Some("inputs_changed"),
                    _ if why.starts_with("downstream of ") => Some("dependency_changed"),
                    _ if why.starts_with("differs from the build in ") => Some("other_checkout"),
                    _ => None,
                });
                let mut safe = numbers(cause, "count compile_ms");
                safe["category"] = json!(category);
                safe["failure"] = json!(cause["failure"].as_bool());
                safe["os_error_code"] = json!(cause["why"].as_str().and_then(os_error_code));
                safe
            })
            .collect::<Vec<_>>()
    });
    let fallback_count = |label| fallback.and_then(|text| sccache_count(text, label));
    json!({
        "schema_version": 1,
        "kache": {
            "schema_version": primary["schema_version"].as_u64(),
            "counts": numbers(&primary, "local_hits prefetch_hits remote_hits dups misses errors total_elapsed_ms hit_elapsed_ms miss_elapsed_ms time_saved_ms"),
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

/// Retain bounded sanitized key events from the observed runtime only.
#[must_use]
pub fn history(runtime: &Path) -> Option<Value> {
    let file = fs::File::open(runtime.join("events.jsonl")).ok()?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(INPUT_LIMIT + 1).ok()?)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() > INPUT_LIMIT;
    bytes.truncate(INPUT_LIMIT);
    let events: Vec<Value> = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    let invalid_records = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count()
        .saturating_sub(events.len());
    let records: Vec<_> = retained_records(&events)
        .iter()
        .map(|event| {
            let mut safe = numbers(
                event,
                "schema elapsed_ms compile_time_ms key_ms lookup_ms restore_ms store_ms",
            );
            for (field, len) in [("cache_key", 64), ("unit_id", 16)] {
                safe[field] = json!(event[field].as_str().and_then(|s| hex(s, len)));
            }
            safe["key_externs_recorded"] = json!(event["key_externs_recorded"].as_bool());
            safe["store_failed"] =
                json!(event["store_error"].as_str().map(|error| !error.is_empty()));
            safe["store_failure_category"] = json!(
                event["store_error"]
                    .as_str()
                    .filter(|error| !error.is_empty())
                    .map(store_failure_category)
            );
            safe["store_os_error_code"] =
                json!(event["store_error"].as_str().and_then(os_error_code));
            safe["key_diff"] = json!(event["key_diff"].as_array().map(|groups| {
                groups
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|group| key_groups().any(|known| known == *group))
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
            safe["key_fields"] = key_groups()
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
        json!({"scope": "job_runtime_only", "prior_jobs": null, "invalid_records": invalid_records, "truncated": truncated || events.len() > records.len() || invalid_records > 0, "records": records}),
    )
}

fn retained_records(records: &[Value]) -> &[Value] {
    const LIMIT: usize = 128;
    &records[..records.len().min(LIMIT)]
}

fn key_groups() -> impl Iterator<Item = &'static str> {
    const GROUPS: &str = "compiler args sources env_deps externs link env_cfg remap crate";
    GROUPS.split_whitespace()
}
