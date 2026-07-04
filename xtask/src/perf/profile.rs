use std::{
    collections::{BTreeMap, btree_map::Entry},
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    common::project::ProjectConfig,
    perf::{
        lanes::{Lane, RunPaths, sanitize},
        list::nextest_list,
        slow::SlowReport,
    },
};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct IsolatedRun {
    pub(crate) suite: String,
    pub(crate) name: String,
    pub(crate) secs: f64,
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) profile_path: PathBuf,
}

pub(crate) struct ProfileParams {
    pub(crate) data_dir: PathBuf,
    pub(crate) run_id: String,
    pub(crate) target_root: PathBuf,
    pub(crate) lane: String,
    pub(crate) timeout_secs: u64,
    pub(crate) limit: Option<usize>,
}

pub(crate) fn samply_command(out: &Path, binary: &Path, test: &str) -> Command {
    let mut cmd = Command::new("samply");
    cmd.arg("record")
        .arg("--unstable-presymbolicate")
        .arg("--save-only")
        .arg("--output")
        .arg(out)
        .arg("--")
        .arg(binary)
        .arg(test)
        .arg("--exact")
        .arg("--nocapture");
    cmd
}

#[derive(Debug, Deserialize)]
struct SymbolSidecar {
    string_table: Vec<String>,
    data: Vec<SymbolLibrary>,
}

#[derive(Debug, Deserialize)]
struct SymbolLibrary {
    debug_name: String,
    code_id: String,
    symbol_table: Vec<SymbolEntry>,
    known_addresses: Vec<(u64, usize)>,
}

#[derive(Debug, Deserialize)]
struct SymbolEntry {
    symbol: usize,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct LibKey {
    debug_name: String,
    code_id: String,
}

impl LibKey {
    fn new(debug_name: &str, code_id: &str) -> Self {
        Self {
            debug_name: debug_name.to_owned(),
            code_id: code_id.to_ascii_lowercase(),
        }
    }
}

fn symbolicate_profile_file(profile_path: &Path) -> Result<usize> {
    let symbols_path = profile_path.with_extension("syms.json");
    let symbols_json = fs::read_to_string(&symbols_path)
        .with_context(|| format!("read {}", symbols_path.display()))?;
    let symbols: SymbolSidecar = serde_json::from_str(&symbols_json)
        .with_context(|| format!("parse {}", symbols_path.display()))?;
    let profile_json = fs::read_to_string(profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: Value = serde_json::from_str(&profile_json)
        .with_context(|| format!("parse {}", profile_path.display()))?;
    let replaced = symbolicate_profile_json_value(&mut profile, &symbols)
        .with_context(|| format!("symbolicate {}", profile_path.display()))?;
    let json = serde_json::to_vec(&profile).context("serialize symbolicated gecko profile")?;
    fs::write(profile_path, json).with_context(|| format!("write {}", profile_path.display()))?;
    Ok(replaced)
}

fn symbolicate_profile_json_value(profile: &mut Value, symbols: &SymbolSidecar) -> Result<usize> {
    let symbol_lookup = symbol_lookup_by_lib(symbols)?;
    let profile_libs = profile_libs(profile)?;
    let threads = profile
        .get_mut("threads")
        .and_then(Value::as_array_mut)
        .context("profile threads must be an array")?;
    let mut replaced = 0;
    for thread in threads {
        let function_symbols = function_symbols(thread, &profile_libs, &symbol_lookup)?;
        replaced += apply_function_symbols(thread, &function_symbols)?;
    }
    Ok(replaced)
}

fn symbol_lookup_by_lib(
    symbols: &SymbolSidecar,
) -> Result<BTreeMap<LibKey, BTreeMap<u64, String>>> {
    let mut lookup = BTreeMap::new();
    for lib in &symbols.data {
        let key = LibKey::new(&lib.debug_name, &lib.code_id);
        let lib_lookup = lookup.entry(key).or_insert_with(BTreeMap::new);
        for (rva, symbol_index) in &lib.known_addresses {
            let symbol = lib
                .symbol_table
                .get(*symbol_index)
                .with_context(|| format!("sidecar symbol index {symbol_index} out of range"))?;
            let name = symbols
                .string_table
                .get(symbol.symbol)
                .with_context(|| format!("sidecar string index {} out of range", symbol.symbol))?;
            match lib_lookup.entry(*rva) {
                Entry::Vacant(entry) => {
                    entry.insert(name.clone());
                }
                Entry::Occupied(entry) if entry.get() == name => {}
                Entry::Occupied(_) => {
                    bail!("sidecar maps address {rva} to multiple symbols");
                }
            }
        }
    }
    Ok(lookup)
}

fn profile_libs(profile: &Value) -> Result<Vec<Option<LibKey>>> {
    let libs = profile
        .get("libs")
        .and_then(Value::as_array)
        .context("profile libs must be an array")?;
    Ok(libs
        .iter()
        .map(|lib| {
            let debug_name = lib.get("debugName").and_then(Value::as_str)?;
            let code_id = lib.get("codeId").and_then(Value::as_str)?;
            Some(LibKey::new(debug_name, code_id))
        })
        .collect())
}

fn function_symbols(
    thread: &Value,
    profile_libs: &[Option<LibKey>],
    symbol_lookup: &BTreeMap<LibKey, BTreeMap<u64, String>>,
) -> Result<BTreeMap<usize, String>> {
    let resource_libs = array_at(thread, &["resourceTable", "lib"])?;
    let frame_addresses = array_at(thread, &["frameTable", "address"])?;
    let frame_funcs = array_at(thread, &["frameTable", "func"])?;
    let func_resources = array_at(thread, &["funcTable", "resource"])?;
    if frame_addresses.len() != frame_funcs.len() {
        bail!("frameTable address and func lengths differ");
    }

    let mut names = BTreeMap::new();
    for (frame_idx, frame_func) in frame_funcs.iter().enumerate() {
        let Some(address) = frame_addresses[frame_idx].as_u64() else {
            continue;
        };
        let Some(func_idx) = value_as_usize(frame_func, "frameTable.func")? else {
            continue;
        };
        let Some(resource_value) = func_resources.get(func_idx) else {
            continue;
        };
        let Some(resource_idx) = value_as_usize(resource_value, "funcTable.resource")? else {
            continue;
        };
        let Some(lib_value) = resource_libs.get(resource_idx) else {
            continue;
        };
        let Some(lib_idx) = value_as_usize(lib_value, "resourceTable.lib")? else {
            continue;
        };
        let Some(Some(lib_key)) = profile_libs.get(lib_idx) else {
            continue;
        };
        let Some(lib_symbols) = symbol_lookup.get(lib_key) else {
            continue;
        };
        let Some(name) = lib_symbols.get(&address) else {
            continue;
        };
        match names.entry(func_idx) {
            Entry::Vacant(entry) => {
                entry.insert(name.clone());
            }
            Entry::Occupied(entry) if entry.get() == name => {}
            Entry::Occupied(_) => {
                bail!("profile function {func_idx} maps to multiple symbols");
            }
        }
    }
    Ok(names)
}

fn apply_function_symbols(thread: &mut Value, names: &BTreeMap<usize, String>) -> Result<usize> {
    let mut assignments = BTreeMap::new();
    {
        let string_array = thread
            .pointer_mut("/stringArray")
            .and_then(Value::as_array_mut)
            .context("thread stringArray must be an array")?;
        let mut string_indexes: BTreeMap<String, usize> = string_array
            .iter()
            .enumerate()
            .filter_map(|(idx, value)| value.as_str().map(|string| (string.to_owned(), idx)))
            .collect();
        for (func_idx, name) in names {
            let name_idx = intern_string(string_array, &mut string_indexes, name);
            assignments.insert(*func_idx, name_idx);
        }
    }

    let func_names = thread
        .pointer_mut("/funcTable/name")
        .and_then(Value::as_array_mut)
        .context("thread funcTable.name must be an array")?;
    let mut replaced = 0;
    for (func_idx, name_idx) in assignments {
        let current = func_names
            .get_mut(func_idx)
            .with_context(|| format!("funcTable.name index {func_idx} out of range"))?;
        let encoded = u64::try_from(name_idx).context("string index does not fit u64")?;
        if current.as_u64() != Some(encoded) {
            *current = Value::from(encoded);
            replaced += 1;
        }
    }
    Ok(replaced)
}

fn array_at<'a>(value: &'a Value, path: &[&str]) -> Result<&'a Vec<Value>> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .with_context(|| format!("missing profile field {}", path.join(".")))?;
    }
    current
        .as_array()
        .with_context(|| format!("profile field {} must be an array", path.join(".")))
}

fn value_as_usize(value: &Value, field: &str) -> Result<Option<usize>> {
    let Some(index) = value.as_u64() else {
        return Ok(None);
    };
    usize::try_from(index)
        .with_context(|| format!("{field} index does not fit usize"))
        .map(Some)
}

fn intern_string(
    string_array: &mut Vec<Value>,
    string_indexes: &mut BTreeMap<String, usize>,
    string: &str,
) -> usize {
    match string_indexes.entry(string.to_owned()) {
        Entry::Occupied(entry) => *entry.get(),
        Entry::Vacant(entry) => {
            let idx = string_array.len();
            string_array.push(Value::from(string));
            entry.insert(idx);
            idx
        }
    }
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<(Option<i32>, bool)> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("poll samply child")? {
            return Ok((status.code(), false));
        }
        if start.elapsed() >= timeout {
            child.kill().context("kill timed-out samply child")?;
            let _ = child.wait();
            return Ok((None, true));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(crate) fn run(params: &ProfileParams) -> Result<()> {
    let paths = RunPaths::new(&params.data_dir, &params.run_id);
    let slow_json_path = paths.slow_json();
    let slow_json = fs::read_to_string(&slow_json_path)
        .with_context(|| format!("read {}", slow_json_path.display()))?;
    let slow: SlowReport = serde_json::from_str(&slow_json).context("parse slow.json")?;
    let Some(lane) = Lane::by_name(&params.lane) else {
        bail!("unknown lane `{}`", params.lane);
    };
    let project = ProjectConfig::load(Path::new("."))?;
    let target_dir = params.target_root.join(&params.lane);
    let suites = nextest_list(&project, lane.flash, lane.backend, &target_dir)?;
    let out_root = paths.profiles_dir(&params.lane);
    let mut isolated = Vec::new();
    let selected = slow.tests.iter().take(params.limit.unwrap_or(usize::MAX));
    for test in selected {
        let Some(suite) = suites.get(&test.suite) else {
            println!(
                "[perf profile] SKIP {} {} (suite not in list)",
                test.suite, test.name
            );
            continue;
        };
        let dir = out_root.join(sanitize(&test.suite));
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let out = dir.join(format!("{}.profile.json", sanitize(&test.name)));
        let mut cmd = samply_command(&out, &suite.binary_path, &test.name);
        cmd.current_dir(&suite.cwd);
        let clock = Instant::now();
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                bail!("samply not found on PATH - install with `cargo install samply`")
            }
            Err(err) => return Err(err).context("spawn samply"),
        };
        let (exit_code, timed_out) =
            wait_with_timeout(&mut child, Duration::from_secs(params.timeout_secs))?;
        let secs = clock.elapsed().as_secs_f64();
        let symbolicated = if timed_out || !out.is_file() {
            0
        } else {
            symbolicate_profile_file(&out)?
        };
        println!(
            "[perf profile] {} {}: {secs:.1}s exit={exit_code:?} timed_out={timed_out} symbolicated={symbolicated}",
            test.suite, test.name
        );
        isolated.push(IsolatedRun {
            suite: test.suite.clone(),
            name: test.name.clone(),
            secs,
            exit_code,
            timed_out,
            profile_path: out,
        });
    }
    fs::create_dir_all(&out_root).with_context(|| format!("create {}", out_root.display()))?;
    let json = serde_json::to_vec_pretty(&isolated).context("serialize isolated runs")?;
    fs::write(out_root.join("isolated.json"), json).context("write isolated.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn samply_command_shape() {
        let cmd = samply_command(
            Path::new("/out/p.profile.json"),
            Path::new("/bin/suite_light-abc"),
            "offline::gapless",
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "samply");
        assert_eq!(
            args,
            [
                "record",
                "--unstable-presymbolicate",
                "--save-only",
                "--output",
                "/out/p.profile.json",
                "--",
                "/bin/suite_light-abc",
                "offline::gapless",
                "--exact",
                "--nocapture"
            ]
        );
    }

    #[test]
    fn symbolicates_function_names_from_samply_sidecar() {
        let mut profile = serde_json::json!({
            "libs": [
                {
                    "debugName": "kithara_bin",
                    "codeId": "ABCDEF"
                },
                {
                    "debugName": "libsystem_kernel.dylib",
                    "codeId": "123456"
                }
            ],
            "threads": [{
                "resourceTable": {
                    "lib": [0, 1]
                },
                "frameTable": {
                    "address": [4660, 4660],
                    "func": [0, 1]
                },
                "funcTable": {
                    "name": [0, 0],
                    "resource": [0, 1]
                },
                "stringArray": [
                    "0x1234",
                    "kithara_bin",
                    "libsystem_kernel.dylib"
                ]
            }]
        });
        let symbols: SymbolSidecar = serde_json::from_value(serde_json::json!({
            "string_table": [
                "UNKNOWN",
                "kithara_mod::do_work",
                "__psynch_cvwait"
            ],
            "data": [
                {
                    "debug_name": "kithara_bin",
                    "code_id": "abcdef",
                    "symbol_table": [
                        { "symbol": 1 }
                    ],
                    "known_addresses": [[4660, 0]]
                },
                {
                    "debug_name": "libsystem_kernel.dylib",
                    "code_id": "123456",
                    "symbol_table": [
                        { "symbol": 2 }
                    ],
                    "known_addresses": [[4660, 0]]
                }
            ]
        }))
        .unwrap();

        let replaced = symbolicate_profile_json_value(&mut profile, &symbols).unwrap();

        assert_eq!(replaced, 2);
        let thread = &profile["threads"][0];
        let strings = thread["stringArray"].as_array().unwrap();
        let names = thread["funcTable"]["name"].as_array().unwrap();
        let first_name = usize::try_from(names[0].as_u64().unwrap()).unwrap();
        let second_name = usize::try_from(names[1].as_u64().unwrap()).unwrap();
        assert_eq!(strings[first_name], "kithara_mod::do_work");
        assert_eq!(strings[second_name], "__psynch_cvwait");
        assert_eq!(strings[0], "0x1234");
    }
}
