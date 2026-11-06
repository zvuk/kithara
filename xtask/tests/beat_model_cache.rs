use std::{
    env, fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::SystemTime,
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo test package directory"))
        .parent()
        .expect("xtask has a workspace root")
        .to_path_buf()
}

mod consts {
    pub(super) const FILE: &str = "test_model.onnx";
    pub(super) const BYTES: &[u8] = b"pinned model bytes for build-script contracts";
    pub(super) const CORRUPT: &[u8] = b"corrupt cached model";
    pub(super) const PROBE: &str = "script::probe";
}

mod script {
    use std::io::{Write, stdout};

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../crates/kithara-beat/build.rs"
    ));

    #[test]
    #[ignore = "run: just test run --lane=tooling -p xtask --test beat_model_cache"]
    fn probe() {
        main();
        let mode = env::var("KITHARA_TEST_MODEL_MODE").expect("probe mode");
        if mode == "cache-dir" {
            println!("model-cache={}", cache_dir().display());
            return;
        }
        let cache = PathBuf::from(env::var_os("KITHARA_BEAT_MODEL_CACHE").expect("probe cache"));
        let url = env::var("KITHARA_TEST_MODEL_URL").expect("local source URL");
        let hash = env::var("KITHARA_TEST_MODEL_HASH").expect("pinned source hash");
        if mode == "fetch" {
            println!("probe-ready");
            stdout().flush().expect("announce lock contender");
            println!(
                "model-accepted={}",
                fetch(&cache, &cache.join(super::consts::FILE), &url, &hash)
            );
        } else {
            assert_eq!(mode, "resolve");
            let model = Model {
                env: "KITHARA_TEST_EMBED_MODEL",
                file: super::consts::FILE,
                source: Some((
                    Box::leak(url.into_boxed_str()),
                    Box::leak(hash.into_boxed_str()),
                )),
            };
            resolve(&cache, &model);
        }
    }
}

struct Fixture {
    _temp: TempDir,
    cache: PathBuf,
    out: PathBuf,
    source: PathBuf,
    temporary: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("model fixture directory");
        let cache = temp.path().join("persistent-source-cache");
        let out = temp.path().join("build/out");
        let source = temp.path().join("source.onnx");
        let temporary = temp.path().join("temporary");
        for path in [&cache, &out, &temporary] {
            fs::create_dir_all(path).expect("model fixture paths");
        }
        fs::write(&source, consts::BYTES).expect("local pinned source");
        Self {
            _temp: temp,
            cache,
            out,
            source,
            temporary,
        }
    }

    fn command(&self, mode: &str, out: &Path) -> Command {
        let mut command = Command::new(env::current_exe().expect("model contract executable"));
        command
            .args([
                "--exact",
                consts::PROBE,
                "--ignored",
                "--nocapture",
                "--format=terse",
            ])
            .env_remove("CARGO_FEATURE_EMBED_MODEL")
            .env_remove("CARGO_FEATURE_EMBED_SMALL_MODEL")
            .env_remove("CARGO_FEATURE_EMBED_FULL_MODEL")
            .env_remove("CARGO_FEATURE_EMBED_FULL_INT8_MODEL")
            .env("KITHARA_TEST_MODEL_MODE", mode)
            .env("KITHARA_BEAT_MODEL_CACHE", &self.cache)
            .env(
                "KITHARA_TEST_MODEL_URL",
                reqwest::Url::from_file_path(&self.source)
                    .expect("local model source URL")
                    .as_str(),
            )
            .env(
                "KITHARA_TEST_MODEL_HASH",
                hex::encode(Sha256::digest(consts::BYTES)),
            )
            .env("OUT_DIR", out)
            .env("TMPDIR", &self.temporary)
            .env("TMP", &self.temporary)
            .env("TEMP", &self.temporary)
            .stdin(Stdio::null());
        command
    }

    fn resolve(&self, out: &Path) -> String {
        stdout(
            self.command("resolve", out)
                .output()
                .expect("resolve model"),
        )
    }

    fn cached_path(&self) -> PathBuf {
        self.cache.join(consts::FILE)
    }

    fn cargo_fixture(&self) -> PathBuf {
        let repository = workspace_root();
        let workspace: toml::Table = fs::read_to_string(repository.join("Cargo.toml"))
            .expect("workspace manifest")
            .parse()
            .expect("workspace dependency declarations");
        let dependencies: toml::Table = ["fs4", "hex", "sha2"]
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    workspace["workspace"]["dependencies"][name].clone(),
                )
            })
            .collect();
        let root = self
            .cache
            .parent()
            .expect("fixture directory")
            .join("cargo-fixture");
        fs::create_dir_all(root.join("src")).expect("Cargo fixture source directory");
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"beat_cache_fixture\"\nversion = \"0.0.0\"\n\
                 edition = \"2024\"\n\n[workspace]\nresolver = \"2\"\n\n\
                 [build-dependencies]\n{}",
                toml::to_string(&dependencies).expect("existing build dependency declarations")
            ),
        )
        .expect("Cargo fixture manifest");
        fs::copy(repository.join("Cargo.lock"), root.join("Cargo.lock"))
            .expect("seed fixture with the workspace dependency lock");
        let owner = repository.join("crates/kithara-beat/build.rs");
        let owner = owner.to_str().expect("build-script source path");
        let url = reqwest::Url::from_file_path(&self.source).expect("local model source URL");
        let hash = hex::encode(Sha256::digest(consts::BYTES));
        fs::write(
            root.join("build.rs"),
            format!(
                r#"mod script {{
    include!({owner:?});

    pub fn run() {{
        main();
        resolve(&cache_dir(), &Model {{
            env: "KITHARA_TEST_EMBED_MODEL",
            file: {file:?},
            source: Some(({url:?}, {hash:?})),
        }});
    }}
}}

fn main() {{
    use std::io::Write;

    script::run();
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out.join("resolver-runs"))
        .expect("resolver execution ledger")
        .write_all(b"run\n")
        .expect("record resolver execution");
}}
"#,
                file = consts::FILE,
                url = url.as_str(),
            ),
        )
        .expect("Cargo build script including the production resolver");
        fs::write(
            root.join("src/lib.rs"),
            "pub const MODEL: &[u8] = include_bytes!(env!(\"KITHARA_TEST_EMBED_MODEL\"));\n",
        )
        .expect("library embeds the resolved model");
        root
    }

    fn cargo_build(&self, root: &Path) -> Vec<serde_json::Value> {
        let pins: toml::Table = fs::read_to_string(workspace_root().join(".config/ci-pins.toml"))
            .expect("repository CI pins")
            .parse()
            .expect("CI toolchain declarations");
        let output = Command::new("cargo")
            .args(["build", "--offline", "--message-format=json"])
            .current_dir(root)
            .env(
                "RUSTUP_TOOLCHAIN",
                pins["nightly_toolchain"].as_str().expect("pinned nightly"),
            )
            .env("CARGO_UNSTABLE_CHECKSUM_FRESHNESS", "true")
            .env("CARGO_TARGET_DIR", root.join("target"))
            .env("KITHARA_BEAT_MODEL_CACHE", &self.cache)
            .env_remove("CARGO_BUILD_TARGET")
            .env_remove("RUSTC")
            .env_remove("CARGO_FEATURE_EMBED_MODEL")
            .env_remove("CARGO_FEATURE_EMBED_SMALL_MODEL")
            .env_remove("CARGO_FEATURE_EMBED_FULL_MODEL")
            .env_remove("CARGO_FEATURE_EMBED_FULL_INT8_MODEL")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env("TMPDIR", &self.temporary)
            .env("TMP", &self.temporary)
            .env("TEMP", &self.temporary)
            .stdin(Stdio::null())
            .output()
            .expect("actual checksum Cargo build");
        stdout(output)
            .lines()
            .map(|line| serde_json::from_str(line).expect("Cargo JSON message"))
            .collect()
    }
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("build script emits UTF-8 directives")
}

fn embedded_path(output: &str) -> PathBuf {
    output
        .lines()
        .find_map(|line| line.strip_prefix("cargo::rustc-env=KITHARA_TEST_EMBED_MODEL="))
        .map(PathBuf::from)
        .expect("validated model is named to the compiler")
}

fn assert_owned_snapshot(output: &str, out: &Path, cache: &Path) -> PathBuf {
    let embedded = embedded_path(output);
    assert!(
        embedded.starts_with(out),
        "embed path must be owned by OUT_DIR: {embedded:?}"
    );
    assert_eq!(
        fs::read(&embedded).expect("owned model snapshot"),
        consts::BYTES
    );
    for path in output
        .lines()
        .filter_map(|line| line.strip_prefix("cargo::rerun-if-changed="))
    {
        assert!(
            !Path::new(path).starts_with(cache),
            "a cache eviction must not become a Cargo input change: {path}"
        );
        assert!(
            !Path::new(path).starts_with(out),
            "fresh build output must not become a build-script rerun input: {path}"
        );
    }
    embedded
}

#[test]
fn an_existing_corrupt_source_is_never_named_to_the_compiler() {
    let fixture = Fixture::new();
    fs::write(fixture.cached_path(), consts::CORRUPT).expect("corrupt cached source");

    let output = fixture.resolve(&fixture.out);

    assert!(
        output.contains("cargo::error="),
        "corruption must be rejected: {output}"
    );
    assert!(!output.contains("cargo::rustc-env=KITHARA_TEST_EMBED_MODEL="));
}

#[test]
fn a_corrupt_source_placed_by_another_lock_holder_is_validated() {
    let fixture = Fixture::new();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(fixture.cached_path().with_extension("onnx.lock"))
        .expect("source lock");
    fs4::FileExt::lock(&lock).expect("hold the model name lock");
    let mut child = fixture
        .command("fetch", &fixture.out)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("contending model fetch");
    let mut reader = BufReader::new(child.stdout.take().expect("contender stdout"));
    let mut output = String::new();
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).expect("contender readiness") > 0);
        output.push_str(&line);
        if line.trim() == "probe-ready" {
            break;
        }
    }
    fs::write(fixture.cached_path(), consts::CORRUPT).expect("another holder places corruption");
    drop(lock);
    reader
        .read_to_string(&mut output)
        .expect("model validation output");
    assert!(child.wait().expect("contender exit").success());

    assert!(
        output.contains("model-accepted=false"),
        "lock does not validate bytes: {output}"
    );
    assert!(output.contains("cargo::error="));
}

#[test]
fn a_fetched_model_has_a_stable_owned_snapshot_on_the_next_resolution() {
    let fixture = Fixture::new();
    let first = fixture.resolve(&fixture.out);
    let embedded = assert_owned_snapshot(&first, &fixture.out, &fixture.cache);
    let modified = fs::metadata(&embedded)
        .expect("owned snapshot metadata")
        .modified()
        .expect("mtime");

    let second = fixture.resolve(&fixture.out);

    assert_eq!(embedded_path(&second), embedded);
    assert_eq!(
        fs::metadata(&embedded)
            .expect("unchanged snapshot metadata")
            .modified()
            .expect("mtime"),
        modified,
        "an unchanged model does not replace its owned snapshot"
    );
    assert_owned_snapshot(&second, &fixture.out, &fixture.cache);
}

#[test]
fn relocation_materializes_identical_bytes_in_the_new_owned_out_dir() {
    let fixture = Fixture::new();
    fs::write(fixture.cached_path(), consts::BYTES).expect("validated cached source");
    let first = fixture.resolve(&fixture.out);
    let original = assert_owned_snapshot(&first, &fixture.out, &fixture.cache);
    let relocated = fixture.out.with_file_name("relocated-out");
    fs::create_dir_all(&relocated).expect("relocated OUT_DIR");

    let second = fixture.resolve(&relocated);

    let copy = assert_owned_snapshot(&second, &relocated, &fixture.cache);
    assert_ne!(copy, original);
    assert_eq!(
        fs::read(copy).expect("relocated bytes"),
        fs::read(original).expect("original bytes")
    );
}

#[test]
fn the_default_source_cache_survives_temporary_directory_relocation() {
    let fixture = Fixture::new();
    let mut command = fixture.command("cache-dir", &fixture.out);
    command.env_remove("KITHARA_BEAT_MODEL_CACHE");
    let first = stdout(command.output().expect("default persistent cache"));
    let first = first
        .lines()
        .find_map(|line| line.strip_prefix("model-cache="))
        .expect("cache path");
    assert!(!Path::new(first).starts_with(&fixture.temporary));
    let other_temp = fixture.temporary.with_file_name("other-temporary");
    fs::create_dir_all(&other_temp).expect("relocated temporary directory");
    command
        .env("TMPDIR", &other_temp)
        .env("TMP", &other_temp)
        .env("TEMP", &other_temp);
    let second = stdout(command.output().expect("relocated default cache"));
    let second = second
        .lines()
        .find_map(|line| line.strip_prefix("model-cache="))
        .expect("cache path");
    assert_eq!(
        first, second,
        "OS temporary cleanup does not move the source cache"
    );
}

#[test]
fn a_fresh_fetch_and_source_cache_churn_rebuild_zero_units_on_the_next_cargo_build() {
    let fixture = Fixture::new();
    let root = fixture.cargo_fixture();
    assert!(
        !fixture.cached_path().exists(),
        "the first build must fetch"
    );
    assert!(
        !root.join("target").exists(),
        "the Cargo target starts empty"
    );

    let first = fixture.cargo_build(&root);

    assert!(
        first.iter().any(|message| {
            message["reason"] == "compiler-artifact"
                && message["target"]["name"] == "beat_cache_fixture"
                && !message["fresh"]
                    .as_bool()
                    .expect("Cargo artifact freshness")
        }),
        "the library must compile after the first fetch: {first:?}"
    );
    assert_eq!(
        fs::read(fixture.cached_path()).expect("fetched model source"),
        consts::BYTES
    );
    let build_script = first
        .iter()
        .find(|message| {
            message["reason"] == "build-script-executed"
                && message["env"].as_array().is_some_and(|variables| {
                    variables
                        .iter()
                        .any(|entry| entry[0] == "KITHARA_TEST_EMBED_MODEL")
                })
        })
        .expect("Cargo consumed the resolver's model directive");
    let embedded = build_script["env"]
        .as_array()
        .expect("Cargo build-script environment")
        .iter()
        .find(|entry| entry[0] == "KITHARA_TEST_EMBED_MODEL")
        .and_then(|entry| entry[1].as_str())
        .map(PathBuf::from)
        .expect("resolved model path");
    let out = PathBuf::from(
        build_script["out_dir"]
            .as_str()
            .expect("Cargo build-script OUT_DIR"),
    );
    assert_eq!(
        fs::read(&embedded).expect("embedded model snapshot"),
        consts::BYTES
    );
    let ledger = out.join("resolver-runs");
    assert_eq!(fs::read(&ledger).expect("first resolver run"), b"run\n");
    let lock = fs::read(root.join("Cargo.lock")).expect("resolved fixture dependency lock");
    let assert_fresh = |messages: &[serde_json::Value]| {
        let artifacts: Vec<_> = messages
            .iter()
            .filter(|message| message["reason"] == "compiler-artifact")
            .collect();
        assert!(
            artifacts
                .iter()
                .any(|message| message["target"]["name"] == "beat_cache_fixture")
        );
        let rebuilt: Vec<_> = artifacts
            .iter()
            .filter(|message| {
                !message["fresh"]
                    .as_bool()
                    .expect("Cargo artifact freshness")
            })
            .map(|message| &message["target"]["name"])
            .collect();
        assert!(
            rebuilt.is_empty(),
            "the next checksum Cargo build rebuilt units: {rebuilt:?}"
        );
        assert_eq!(
            fs::read(&ledger).expect("resolver execution ledger"),
            b"run\n"
        );
        assert_eq!(
            fs::read(root.join("Cargo.lock")).expect("unchanged fixture lock"),
            lock
        );
    };

    let second = fixture.cargo_build(&root);
    assert_fresh(&second);

    fs::File::open(fixture.cached_path())
        .expect("cached model to touch")
        .set_modified(SystemTime::now())
        .expect("touch cached model without changing bytes");
    let after_touch = fixture.cargo_build(&root);
    assert_fresh(&after_touch);

    fs::remove_file(fixture.cached_path()).expect("evict only this fixture's source cache");
    let after_eviction = fixture.cargo_build(&root);
    assert_fresh(&after_eviction);
    assert!(
        !fixture.cached_path().exists(),
        "a fresh Cargo build must not refetch"
    );

    fixture.resolve(&out);
    assert_eq!(
        fs::read(fixture.cached_path()).expect("refetched source bytes"),
        consts::BYTES
    );
    let after_refetch = fixture.cargo_build(&root);
    assert_fresh(&after_refetch);

    fs::remove_dir_all(&fixture.cache).expect("evict the fixture's entire cache directory");
    let after_directory_eviction = fixture.cargo_build(&root);
    assert_fresh(&after_directory_eviction);
    assert!(!fixture.cache.exists());
    assert_eq!(
        fs::read(&embedded).expect("snapshot survives source eviction"),
        consts::BYTES
    );
    assert!(
        embedded.starts_with(&out) && out.starts_with(root.join("target")),
        "Cargo owns the embed path: {embedded:?} in {out:?}"
    );
}
