//! Bake `app.yaml` (+ workspace `.env`) into compile-time defaults
//! exposed via `kithara_app::baked`. Secrets referenced by
//! `*_env: $KITHARA_...` are emitted as `obfstr!("<value>")` so the
//! literal does not appear as a plain run of bytes in `strings`
//! output; non-secret inline values are emitted verbatim.
//!
//! Env resolution order for a given key:
//! 1. Real process env (preserves explicit `KEY=foo cargo build`).
//! 2. Workspace `<root>/.env` file (parsed locally — never mutated
//!    into the build process env).
//! 3. Nothing — the provider's secret is replaced with an empty string
//!    and a `cargo:warning=...` is emitted. The binary still compiles,
//!    but the key server will reject the request.
//!
//! `app.yaml` and `.env` are both `rerun-if-changed`.

use std::{collections::HashMap, env, fmt::Write, fs, path::PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
struct AppConfig {
    drm: Drm,
    network: Network,
    playback: Playback,
    playlist: Playlist,
}

#[derive(Deserialize)]
struct Playback {
    crossfade_seconds: f32,
    eq_band_count: usize,
}

#[derive(Deserialize)]
struct Network {
    /// HLS size-estimation probe method. `"head"` (RFC default) or
    /// `"range_get"` for upstreams that reject `HEAD` (e.g. zvuk
    /// stage `/drm/`). See [`kithara::hls::SizeProbeMethod`].
    #[serde(default = "default_size_probe_method")]
    size_probe_method: String,
    /// `Accept-Encoding` algorithms the HTTP client offers. Mapped to
    /// the `kithara_net::Compression` bitflags. Empty list ships as
    /// `Compression::empty()` (Accept-Encoding negotiation disabled).
    #[serde(default = "default_compression")]
    compression: Vec<String>,
    should_accept_invalid_certs: bool,
}

fn default_size_probe_method() -> String {
    "head".to_owned()
}

fn default_compression() -> Vec<String> {
    vec!["gzip", "deflate", "brotli", "zstd"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[derive(Deserialize)]
struct Playlist {
    tracks: Vec<String>,
}

#[derive(Deserialize)]
struct Drm {
    providers: Vec<DrmProvider>,
}

#[derive(Deserialize)]
struct DrmProvider {
    /// Extra HTTP headers attached to every request matched by this
    /// provider (playlist, segments, key fetches). Values starting
    /// with `$` are env references and wrapped with `obfstr!()`;
    /// everything else is shipped as a literal. Use this for
    /// `X-Auth-Token`, `User-Agent`, `X-SP-ZV`, and any other
    /// per-provider header.
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
    /// Env reference of the form `$KITHARA_...`; resolved at build
    /// time and wrapped with `obfstr!()`.
    cipher_env: Option<String>,
    /// Inline non-secret cipher key (ships verbatim in the binary).
    cipher_key: Option<String>,
    /// Per-provider X-Encrypted-Key salt shape. Defaults to the iOS
    /// prod format (8-char lowercase hex). Override per provider when
    /// the upstream WAF expects a different alphabet/length — zvq.me
    /// staging is captured against a 16-char alphanumeric salt.
    #[serde(default)]
    seed: SeedSpec,
    name: String,
    domains: Vec<String>,
}

#[derive(Deserialize)]
struct SeedSpec {
    /// `hex` (0-9 a-f) or `alphanumeric` (a-z A-Z 0-9).
    alphabet: String,
    /// Output salt length in characters.
    length: usize,
}

impl Default for SeedSpec {
    fn default() -> Self {
        Self {
            length: 8,
            alphabet: "hex".into(),
        }
    }
}

impl SeedSpec {
    fn emit_let_seed(&self, provider_name: &str) -> String {
        match self.alphabet.to_ascii_lowercase().as_str() {
            "hex" => {
                assert!(
                    self.length.is_multiple_of(2),
                    "provider `{}` seed.length must be even for alphabet=hex (got {})",
                    provider_name,
                    self.length,
                );
                let bytes = self.length / 2;
                format!(
                    "            let mut seed_bytes = [0u8; {bytes}];\n\
                     ::rand::rng().fill_bytes(&mut seed_bytes);\n\
                     let seed: String = seed_bytes.iter().map(|b| format!(\"{{b:02x}}\")).collect();\n"
                )
            }
            "alphanumeric" => {
                let length = self.length;
                format!(
                    "            let seed: String = ::rand::rng()\n\
                                 .sample_iter(::rand::distr::Alphanumeric)\n\
                                 .take({length})\n\
                                 .map(char::from)\n\
                                 .collect();\n"
                )
            }
            other => panic!(
                "provider `{}` seed.alphabet `{other}` not supported (expected `hex` or `alphanumeric`)",
                provider_name
            ),
        }
    }
}

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"));
    let app_yaml_path = manifest_dir.join("app.yaml");
    let workspace_root = manifest_dir.join("..").join("..");
    let dotenv_path = workspace_root.join(".env");

    println!("cargo:rerun-if-changed={}", app_yaml_path.display());
    println!("cargo:rerun-if-changed={}", dotenv_path.display());

    let yaml_src = fs::read_to_string(&app_yaml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", app_yaml_path.display()));
    let app: AppConfig = serde_yml::from_str(&yaml_src)
        .unwrap_or_else(|e| panic!("parse {}: {e}", app_yaml_path.display()));

    let env_map = load_env(&dotenv_path);

    for provider in &app.drm.providers {
        if let Some(name) = provider.cipher_env.as_deref().and_then(parse_env_ref) {
            println!("cargo:rerun-if-env-changed={name}");
        }
        for value in provider.headers.values() {
            if let Some(name) = parse_env_ref(value) {
                println!("cargo:rerun-if-env-changed={name}");
            }
        }
    }

    let mut code = String::new();
    emit_scalars(&mut code, &app);
    emit_tracks(&mut code, &app.playlist.tracks);
    emit_registry(&mut code, &app.drm.providers, &env_map);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let out_path = out_dir.join("app_config_baked.rs");
    fs::write(&out_path, code).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
}

fn emit_scalars(code: &mut String, app: &AppConfig) {
    writeln!(
        code,
        "pub const BAKED_CROSSFADE_SECONDS: f32 = {:?};",
        app.playback.crossfade_seconds
    )
    .expect("write to String never fails");
    writeln!(
        code,
        "pub const BAKED_EQ_BAND_COUNT: usize = {};",
        app.playback.eq_band_count
    )
    .expect("write to String never fails");
    writeln!(
        code,
        "pub const BAKED_SHOULD_ACCEPT_INVALID_CERTS: bool = {};",
        app.network.should_accept_invalid_certs
    )
    .expect("write to String never fails");
    writeln!(
        code,
        "pub const BAKED_COMPRESSION: ::kithara::net::Compression = {};",
        compression_expr(&app.network.compression)
    )
    .expect("write to String never fails");
    writeln!(
        code,
        "pub const BAKED_SIZE_PROBE_METHOD: ::kithara::hls::SizeProbeMethod = {};",
        size_probe_method_expr(&app.network.size_probe_method)
    )
    .expect("write to String never fails");
}

fn size_probe_method_expr(method: &str) -> &'static str {
    match method.to_ascii_lowercase().as_str() {
        "head" => "::kithara::hls::SizeProbeMethod::Head",
        "range_get" | "range-get" => "::kithara::hls::SizeProbeMethod::RangeGet",
        other => {
            panic!("unknown network.size_probe_method `{other}` (expected `head` or `range_get`)")
        }
    }
}

fn compression_expr(algos: &[String]) -> String {
    let parts: Vec<&'static str> = algos
        .iter()
        .map(|name| match name.to_ascii_lowercase().as_str() {
            "gzip" => "::kithara::net::Compression::GZIP",
            "deflate" => "::kithara::net::Compression::DEFLATE",
            "brotli" | "br" => "::kithara::net::Compression::BROTLI",
            "zstd" => "::kithara::net::Compression::ZSTD",
            other => panic!("unknown compression algorithm `{other}` in network.compression"),
        })
        .collect();
    parts.split_first().map_or_else(
        || "::kithara::net::Compression::empty()".to_string(),
        |(head, tail)| {
            tail.iter().fold((*head).to_string(), |acc, next| {
                format!("{acc}.union({next})")
            })
        },
    )
}

fn emit_tracks(code: &mut String, tracks: &[String]) {
    code.push_str("pub const BAKED_TRACKS: &[&str] = &[\n");
    for track in tracks {
        writeln!(code, "    {track:?},").expect("write to String never fails");
    }
    code.push_str("];\n");
}

fn emit_registry(code: &mut String, providers: &[DrmProvider], env_map: &HashMap<String, String>) {
    // Each provider emits its own `KeyRequestFactory` closure that
    // rebuilds the X-Encrypted-Key salt on EVERY key request, using
    // the alphabet/length declared per-provider in `app.yaml`. iOS
    // `HLSAes128Service.AES128ResourceLoader` ships an 8-char
    // lowercase-hex salt for zvuk.com WAF, while zvq.me staging
    // accepts a 16-char alphanumeric salt — both formats coexist via
    // [`SeedSpec`].
    code.push_str(
        "#[must_use]\n\
         pub fn build_baked_drm_registry() -> ::kithara_drm::KeyProcessorRegistry {\n\
         use ::std::{collections::HashMap, sync::Arc};\n\
         use ::kithara_drm::{KeyProcessorRegistry, KeyProcessorRule, KeyRequest, KeyRequestFactory, KeyProcessor, UniqueBinaryCipher};\n\
         use ::bytes::Bytes;\n\
         use ::rand::prelude::*;\n\
         let mut registry = KeyProcessorRegistry::new();\n",
    );
    for provider in providers {
        emit_provider(code, provider, env_map);
    }
    code.push_str("    registry\n}\n");
}

fn emit_provider(code: &mut String, p: &DrmProvider, env_map: &HashMap<String, String>) {
    let cipher_expr = resolve_secret(
        p.cipher_key.as_deref(),
        p.cipher_env.as_deref(),
        env_map,
        &format!("provider `{}` cipher", p.name),
    );
    let domains = p
        .domains
        .iter()
        .map(|d| format!("{d:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut extra_inserts = String::new();
    for (name, value) in &p.headers {
        if name.as_str() == "X-Encrypted-Key" {
            panic!(
                "provider `{}` headers must not set `X-Encrypted-Key` — it is generated per request from the cipher_key",
                p.name
            );
        }
        let label = format!("provider `{}` header `{name}`", p.name);
        let Some(value_expr) = render_template(value, env_map, &label) else {
            continue;
        };
        writeln!(
            extra_inserts,
            "        base_headers.insert({name:?}.to_string(), ({value_expr}).to_string());"
        )
        .expect("write to String never fails");
    }
    let seed_let = p.seed.emit_let_seed(&p.name);
    writeln!(
        code,
        r#"    {{
        let cipher_key: Arc<str> = Arc::from(({cipher_expr}).to_string());
        let factory: KeyRequestFactory = Arc::new(move || {{
{seed_let}            let secret = format!("{{}}{{}}", cipher_key, seed);
            let cipher = UniqueBinaryCipher::new(&secret);
            let mut headers = HashMap::new();
            headers.insert("X-Encrypted-Key".to_string(), seed);
            let processor: KeyProcessor = Arc::new(move |key: Bytes| Ok(cipher.decrypt(&key)));
            KeyRequest::new(headers, processor)
        }});
        let mut base_headers = HashMap::new();
{extra_inserts}        registry.add(
            KeyProcessorRule::for_domains([{domains}], factory)
                .headers(base_headers)
                .build()
        );
    }}"#
    )
    .expect("write to String never fails");
}

/// Strip the leading `$` of an env reference (`$KITHARA_FOO`) and
/// return the bare name. `None` if the value is not a reference.
fn parse_env_ref(value: &str) -> Option<&str> {
    value.strip_prefix('$')
}

/// Required secret: must come from either `inline` (non-secret literal)
/// or `env_ref` (`$KITHARA_...` looked up in `env_map` and wrapped with
/// `obfstr!()`). Panics if both or neither are set.
fn resolve_secret(
    inline: Option<&str>,
    env_ref: Option<&str>,
    env_map: &HashMap<String, String>,
    label: &str,
) -> String {
    match (inline, env_ref) {
        (Some(_), Some(_)) => {
            panic!("{label}: both inline and env reference set — pick one");
        }
        (Some(v), None) => literal_expr(v, false),
        (None, Some(reference)) => {
            let name = parse_env_ref(reference).unwrap_or_else(|| {
                panic!("{label}: env reference must start with `$` (got `{reference}`)")
            });
            env_map.get(name).filter(|v| !v.is_empty()).map_or_else(
                || {
                    println!(
                        "cargo:warning={label}: env `{name}` not set; falling back to empty string"
                    );
                    literal_expr("", false)
                },
                |v| literal_expr(v, true),
            )
        }
        (None, None) => panic!("{label}: neither inline value nor env reference provided"),
    }
}

/// Optional secret: returns an `Option<String>` expression. Same
fn literal_expr(value: &str, obfuscate: bool) -> String {
    if obfuscate {
        format!("::obfstr::obfstr!({value:?})")
    } else {
        format!("{value:?}")
    }
}

/// Render a header value that may contain `${KITHARA_VAR}` substring
/// placeholders, or be exactly `$KITHARA_VAR` (whole-string shorthand).
/// Literal values pass through unchanged. Env-resolved values are
/// wrapped with `obfstr!()`. `None` means the value referenced a
/// missing env var — the caller should drop the header entirely.
fn render_template(value: &str, env_map: &HashMap<String, String>, label: &str) -> Option<String> {
    if !value.contains("${") {
        if let Some(name) = parse_env_ref(value) {
            return env_map.get(name).filter(|v| !v.is_empty()).map_or_else(
                || {
                    println!("cargo:warning={label}: env `{name}` not set; value omitted");
                    None
                },
                |v| Some(literal_expr(v, true)),
            );
        }
        return Some(literal_expr(value, false));
    }
    let mut fmt_str = String::new();
    let mut args: Vec<String> = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let (before, tail) = rest.split_at(start);
        fmt_str.push_str(&before.replace('{', "{{").replace('}', "}}"));
        let tail = &tail[2..];
        let end = tail
            .find('}')
            .unwrap_or_else(|| panic!("{label}: unterminated `${{...` in `{value}`"));
        let name = &tail[..end];
        if let Some(v) = env_map.get(name).filter(|v| !v.is_empty()) {
            fmt_str.push_str("{}");
            args.push(literal_expr(v, true));
        } else {
            println!("cargo:warning={label}: env `{name}` not set; value omitted");
            return None;
        }
        rest = &tail[end + 1..];
    }
    fmt_str.push_str(&rest.replace('{', "{{").replace('}', "}}"));
    Some(format!("format!({fmt_str:?}, {})", args.join(", ")))
}

fn load_env(path: &PathBuf) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = env::vars().collect();
    let Ok(contents) = fs::read_to_string(path) else {
        return map;
    };
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.starts_with("KITHARA_") {
            continue;
        }
        if map.contains_key(key) {
            continue;
        }
        let value = value.trim().trim_matches(|c: char| c == '"' || c == '\'');
        map.insert(key.to_string(), value.to_string());
    }
    map
}
