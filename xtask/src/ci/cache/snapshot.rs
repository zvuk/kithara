use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use kithara_devtools::lease;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tracing::info;

use super::{current_client_environment, require_success};
use crate::consts;

struct Snapshot;

impl Snapshot {
    const SCHEMA: &str = "kithara-target-snapshot-v4";
    const PREFIX: &str = "target-snapshots";

    fn object(fingerprint: &str, checksum: &str) -> String {
        format!("{}/{fingerprint}/{checksum}.tar", Self::PREFIX)
    }
}

/// The dependency sources a job would otherwise fetch from the public
/// internet. Unlike a target snapshot this layer is content-addressed by
/// `Cargo.lock` alone: sources do not depend on the toolchain, the flags, the
/// lane or the checkout path, and they are identical on every platform, so one
/// object serves both fleets. Each scope's default branch publishes it into
/// the scope's own bucket, and the scope's branches read it from there.
struct Sources;

impl Sources {
    const SCHEMA: &str = "kithara-source-snapshot-v1";
    const PREFIX: &str = "source-snapshots";
    /// Records which layer a `CARGO_HOME` already carries, so a job that
    /// already has it neither downloads it again nor unpacks over a live one.
    const MARKER: &str = ".kithara-source-snapshot";
    /// The directories carried, relative to `CARGO_HOME`. `registry` holds the
    /// index, the downloaded `.crate` files and their unpacked sources; `git`
    /// holds the bare databases and the checkouts cargo builds from.
    const PATHS: [&str; 5] = [
        "registry/cache",
        "registry/index",
        "registry/src",
        "git/db",
        "git/checkouts",
    ];

    fn object(fingerprint: &str, checksum: &str) -> String {
        format!("{}/{fingerprint}/{checksum}.tar", Self::PREFIX)
    }
}

#[derive(Debug, Args)]
pub(super) struct SnapshotArgs {
    #[command(subcommand)]
    command: SnapshotCommand,
}

#[derive(Debug, Subcommand)]
enum SnapshotCommand {
    /// Print the target snapshot key for this lane and toolchain.
    Fingerprint {
        #[arg(long)]
        lane: String,
        #[arg(long)]
        profile: String,
        #[arg(long, default_value = "host")]
        target: String,
    },
    /// Restore an immutable snapshot into an empty private target directory.
    Restore {
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        fingerprint: String,
    },
    /// Publish an immutable target snapshot in this job's cache scope.
    Publish {
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        fingerprint: String,
    },
}

pub(super) fn run(args: &SnapshotArgs) -> Result<()> {
    match &args.command {
        SnapshotCommand::Fingerprint {
            lane,
            profile,
            target,
        } => {
            let cargo_home = env::var_os("CARGO_HOME").map_or_else(PathBuf::new, PathBuf::from);
            println!(
                "{}",
                fingerprint(lane, profile, target, Path::new("."), &cargo_home)?
            );
            Ok(())
        }
        SnapshotCommand::Restore {
            target,
            fingerprint,
        } => restore(target, fingerprint, Path::new("rc")).map(|_| ()),
        SnapshotCommand::Publish {
            target,
            fingerprint,
        } => publish(target, fingerprint, Path::new("rc")),
    }
}

pub(crate) fn restore_for_lane(
    key: &str,
    target: &Path,
    root: &Path,
    cargo_home: &Path,
    rc: &Path,
) -> Result<Option<String>> {
    if target.is_dir() {
        require_target(target, false)?;
        if !target_is_empty(target)? {
            info!(path = %target.display(), "retained warm target directory");
            return Ok(None);
        }
    }
    let fingerprint = fingerprint(key, "cargo", "host", root, cargo_home)?;
    let restored = restore(target, &fingerprint, rc)?;
    Ok(snapshot_to_publish(fingerprint, restored))
}

fn snapshot_to_publish(fingerprint: String, restored: bool) -> Option<String> {
    (!restored).then_some(fingerprint)
}

pub(crate) fn publish_for_lane(target: &Path, fingerprint: &str, rc: &Path) -> Result<()> {
    publish(target, fingerprint, rc)
}

/// What a restore found. Three outcomes used to arrive as one warning, so a
/// lane whose `Cargo.lock` simply had no object yet read exactly like a lane
/// that could not reach the cache at all.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Restored {
    /// The home already carries this fingerprint; nothing to do.
    AlreadyPresent,
    /// No object exists under this key. The ordinary case on a changed
    /// `Cargo.lock`, and not a failure.
    Absent,
    /// The layer was downloaded and unpacked.
    Layer(String),
}

/// Fill `cargo_home` with the dependency sources this `Cargo.lock` names, so
/// the job compiles instead of fetching.
pub(crate) fn restore_sources(root: &Path, cargo_home: &Path, rc: &Path) -> Result<Restored> {
    let fingerprint = sources_fingerprint(root)?;
    if read_marker(cargo_home)?.as_deref() == Some(fingerprint.as_str()) {
        return Ok(Restored::AlreadyPresent);
    }
    let client = Client::load(rc)?;
    let Some(object) = client.latest(Sources::PREFIX, &fingerprint)? else {
        return Ok(Restored::Absent);
    };
    let expected = checksum_of(&object)?;
    let archive = NamedTempFile::new().context("create source snapshot download")?;
    client.copy_from(&object, archive.path())?;
    ensure!(
        sha256(archive.path())? == expected,
        "source snapshot checksum mismatch"
    );
    verify_archive(archive.path())?;
    fs::create_dir_all(cargo_home)
        .with_context(|| format!("create cargo home {}", cargo_home.display()))?;
    run_command(
        Command::new("tar")
            .args(["--extract", "--zstd", keep_existing()?, "--file"])
            .arg(archive.path())
            .arg("--directory")
            .arg(cargo_home),
        "restore source snapshot",
    )?;
    write_marker(cargo_home, &fingerprint)?;
    info!(%fingerprint, object, "restored dependency sources");
    Ok(Restored::Layer(object))
}

/// Publish the sources this job ended up with into its own scope's bucket.
pub(crate) fn publish_sources(root: &Path, cargo_home: &Path, rc: &Path) -> Result<()> {
    let client = Client::load(rc)?;
    let fingerprint = sources_fingerprint(root)?;
    let present: Vec<&str> = Sources::PATHS
        .into_iter()
        .filter(|path| cargo_home.join(path).is_dir())
        .collect();
    ensure!(
        !present.is_empty(),
        "cargo home carries no sources to publish"
    );
    let archive = NamedTempFile::new().context("create source snapshot archive")?;
    let mut command = Command::new("tar");
    command
        .args(["--create", "--zstd", "--file"])
        .arg(archive.path())
        .arg("--directory")
        .arg(cargo_home);
    for path in present {
        command.arg(path);
    }
    run_command(&mut command, "archive source snapshot")?;
    let checksum = sha256(archive.path())?;
    let object = Sources::object(&fingerprint, &checksum);
    if client.exists(&object)? {
        info!(%fingerprint, %checksum, "source snapshot already exists");
        return Ok(());
    }
    client.copy(archive.path(), &object)?;
    write_marker(cargo_home, &fingerprint)?;
    info!(%fingerprint, %checksum, "published dependency sources");
    Ok(())
}

/// The extraction flag that leaves a file already on disk alone.
///
/// Nothing in this archive is worth overwriting: a registry entry is named by
/// its content, so a file that is already there already holds the right bytes.
/// Leaving it is also what keeps the restore safe beside a neighbour, because
/// the two jobs this host runs at once share one cargo home and this untar does
/// not hold the package-cache lock the neighbour's compile respects. The two
/// tars disagree on which flag says it: the BSD one errors on `--skip-old-files`
/// and the GNU one treats `--keep-old-files` as a demand that nothing collide.
fn keep_existing() -> Result<&'static str> {
    let version = Command::new("tar")
        .arg("--version")
        .output()
        .context("read tar version")?;
    Ok(if version.stdout.starts_with(b"tar (GNU tar)") {
        "--skip-old-files"
    } else {
        "--keep-old-files"
    })
}

/// The lock file is the whole key. It names every crate version and every git
/// revision the build may reach, and nothing else about the job changes what
/// those bytes are.
fn sources_fingerprint(root: &Path) -> Result<String> {
    let lock = root.join("Cargo.lock");
    let bytes = fs::read(&lock).with_context(|| format!("read {}", lock.display()))?;
    let mut hash = Sha256::new();
    for value in [Sources::SCHEMA.as_bytes(), &bytes] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    Ok(hex::encode(hash.finalize()))
}

fn read_marker(cargo_home: &Path) -> Result<Option<String>> {
    let marker = cargo_home.join(Sources::MARKER);
    if !marker.is_file() {
        return Ok(None);
    }
    let value = fs::read_to_string(&marker)
        .with_context(|| format!("read {}", marker.display()))?
        .trim()
        .to_owned();
    Ok(Some(value))
}

fn write_marker(cargo_home: &Path, fingerprint: &str) -> Result<()> {
    let marker = cargo_home.join(Sources::MARKER);
    fs::write(&marker, fingerprint).with_context(|| format!("write {}", marker.display()))
}

/// The object name carries the checksum its bytes must have, which is what
/// makes a published snapshot immutable rather than merely named.
fn checksum_of(object: &str) -> Result<&str> {
    object
        .rsplit_once('/')
        .and_then(|(_, name)| name.strip_suffix(".tar"))
        .context("snapshot object has no checksum name")
}

fn fingerprint(
    lane: &str,
    profile: &str,
    target: &str,
    root: &Path,
    cargo_home: &Path,
) -> Result<String> {
    validate_component(lane, "lane")?;
    validate_component(profile, "profile")?;
    validate_component(target, "target")?;
    let rustc = Command::new("rustc")
        .arg("-vV")
        .output()
        .context("read Rust toolchain identity")?;
    require_success(&rustc, "read Rust toolchain identity")?;
    let rustflags = env::var("RUSTFLAGS").unwrap_or_default();
    let encoded_rustflags = env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let mut hash = Sha256::new();
    #[cfg(target_os = "macos")]
    for (program, args, label) in [
        (
            "xcodebuild",
            ["-version"].as_slice(),
            "read Xcode build identity",
        ),
        (
            "xcrun",
            ["--sdk", "macosx", "--show-sdk-build-version"].as_slice(),
            "read Apple SDK build identity",
        ),
    ] {
        let output = Command::new(program).args(args).output().context(label)?;
        require_success(&output, label)?;
        hash.update((output.stdout.len() as u64).to_le_bytes());
        hash.update(output.stdout);
    }

    for value in [
        Snapshot::SCHEMA.as_bytes(),
        lane.as_bytes(),
        profile.as_bytes(),
        target.as_bytes(),
        &rustc.stdout,
        rustflags.as_bytes(),
        encoded_rustflags.as_bytes(),
        root.as_os_str().as_encoded_bytes(),
        cargo_home.as_os_str().as_encoded_bytes(),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    for path in ["Cargo.lock", ".config/ci-pins.toml"] {
        let bytes =
            fs::read(root.join(path)).with_context(|| format!("read snapshot input {path}"))?;
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(hex::encode(hash.finalize()))
}

fn publish(target: &Path, fingerprint: &str, rc: &Path) -> Result<()> {
    validate_fingerprint(fingerprint)?;
    require_target(target, false)?;
    let archive = NamedTempFile::new().context("create target snapshot archive")?;
    run_command(
        Command::new("tar")
            .args(["--create", "--format=pax", "--zstd", "--file"])
            .arg(archive.path())
            .arg(format!("--exclude={}", lease::FILE))
            .arg(format!(
                "--exclude={heartbeat}",
                heartbeat = lease::HEARTBEAT
            ))
            .arg(format!("--exclude={}", consts::SOURCES_RECORD))
            .arg("--directory")
            .arg(target)
            .arg("."),
        "archive target snapshot",
    )?;
    let checksum = sha256(archive.path())?;
    let object = Snapshot::object(fingerprint, &checksum);
    let client = Client::load(rc)?;
    if client.exists(&object)? {
        info!(%fingerprint, %checksum, "target snapshot already exists");
        return Ok(());
    }
    client.copy(archive.path(), &object)?;
    info!(%fingerprint, %checksum, "published immutable target snapshot");
    Ok(())
}

fn restore(target: &Path, fingerprint: &str, rc: &Path) -> Result<bool> {
    validate_fingerprint(fingerprint)?;
    require_target(target, true)?;
    let client = Client::load(rc)?;
    let Some(object) = client.latest(Snapshot::PREFIX, fingerprint)? else {
        info!(%fingerprint, "no target snapshot exists");
        return Ok(false);
    };
    let expected = checksum_of(&object)?;
    let archive = NamedTempFile::new().context("create target snapshot download")?;
    client.copy_from(&object, archive.path())?;
    ensure!(
        sha256(archive.path())? == expected,
        "target snapshot checksum mismatch"
    );
    verify_archive(archive.path())?;
    run_command(
        Command::new("tar")
            .args(["--extract", "--zstd", "--file"])
            .arg(archive.path())
            .arg("--directory")
            .arg(target),
        "restore target snapshot",
    )?;
    info!(%fingerprint, object, "restored immutable target snapshot");
    Ok(true)
}

fn require_target(target: &Path, may_create: bool) -> Result<()> {
    ensure!(
        target.is_absolute(),
        "target snapshot directory must be absolute"
    );
    if !target.exists() {
        ensure!(may_create, "target snapshot directory does not exist");
        fs::create_dir_all(target)
            .with_context(|| format!("create target snapshot directory {}", target.display()))?;
    }
    ensure!(target.is_dir(), "target snapshot path is not a directory");
    if may_create {
        ensure!(
            target_is_empty(target)?,
            "target snapshot restore requires an empty private target directory except its live job markers"
        );
    }
    Ok(())
}

fn target_is_empty(target: &Path) -> Result<bool> {
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        if !matches!(
            entry.file_name().to_str(),
            Some(name) if name == lease::FILE || name == lease::HEARTBEAT || name == consts::SOURCES_RECORD
        ) || !entry.file_type()?.is_file()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_fingerprint(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "target snapshot fingerprint must be a lowercase SHA-256"
    );
    Ok(())
}

fn validate_component(value: &str, label: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "target snapshot {label} contains unsafe characters"
    );
    Ok(())
}

fn verify_archive(archive: &Path) -> Result<()> {
    let output = Command::new("tar")
        .args(["--list", "--zstd", "--file"])
        .arg(archive)
        .output()
        .context("list target snapshot archive")?;
    require_success(&output, "list target snapshot archive")?;
    for path in String::from_utf8(output.stdout)
        .context("target snapshot archive paths are not UTF-8")?
        .lines()
    {
        let path = Path::new(path);
        ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|component| matches!(component, Component::CurDir | Component::Normal(_))),
            "target snapshot archive contains unsafe path {}",
            path.display()
        );
        let relative: PathBuf = path
            .components()
            .filter(|part| *part != Component::CurDir)
            .collect();
        ensure!(
            ![lease::FILE, lease::HEARTBEAT, consts::SOURCES_RECORD]
                .iter()
                .any(|name| relative == Path::new(name)),
            "snapshot archive contains live build ownership metadata"
        );
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            return Ok(hex::encode(hash.finalize()));
        }
        hash.update(&buffer[..read]);
    }
}

fn run_command(command: &mut Command, what: &str) -> Result<()> {
    let output = command.output().with_context(|| format!("start {what}"))?;
    require_success(&output, what)
}

/// What `rc --json object list` prints: one entry per object or common
/// prefix, each key relative to the bucket. A prefix ends in `/`, so the
/// `.tar` filter already leaves it out.
#[derive(Deserialize)]
struct Listing {
    items: Vec<ListedObject>,
}

#[derive(Deserialize)]
struct ListedObject {
    key: String,
}

struct Client {
    // Cargo fingerprints retain registry source paths under CARGO_HOME. The
    // snapshot therefore has to follow the same trust-scoped bucket as that
    // home or every restored dependency is immediately stale.
    bucket: String,
    endpoint: String,
    environment: BTreeMap<String, String>,
    program: PathBuf,
    configuration: tempfile::TempDir,
}

impl Client {
    /// The exit status `rc` gives an object that does not exist.
    const NOT_FOUND: i32 = 5;

    fn load(program: &Path) -> Result<Self> {
        let environment = current_client_environment()?;
        let endpoint = environment
            .get("SCCACHE_ENDPOINT")
            .cloned()
            .context("cache environment has no endpoint")?;
        let bucket = environment
            .get("SCCACHE_BUCKET")
            .cloned()
            .context("cache environment has no bucket")?;
        Ok(Self {
            bucket,
            endpoint,
            environment,
            program: program.to_owned(),
            configuration: tempfile::tempdir().context("create snapshot client configuration")?,
        })
    }

    fn command(&self) -> Result<Command> {
        let key = self
            .environment
            .get("AWS_ACCESS_KEY_ID")
            .context("cache key missing")?;
        let secret = self
            .environment
            .get("AWS_SECRET_ACCESS_KEY")
            .context("cache secret missing")?;
        let command = || {
            let mut command = Command::new(&self.program);
            command
                .env("HOME", self.configuration.path())
                .env("XDG_CONFIG_HOME", self.configuration.path())
                .env("APPDATA", self.configuration.path());
            command
        };
        let output = command()
            .args(["alias", "set", "snapshot", &self.endpoint, key, secret])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .context("configure snapshot storage client")?;
        require_success(&output, "configure snapshot storage client")?;
        Ok(command())
    }

    /// Only an object that is not there reads as absent. A lookup the store
    /// refused or never answered is a failure, not a reason to upload.
    fn exists(&self, object: &str) -> Result<bool> {
        let mut command = self.command()?;
        let output = command
            .args(["object", "stat"])
            .arg(self.remote(object))
            .output()
            .context("start snapshot lookup")?;
        if output.status.code() == Some(Self::NOT_FOUND) {
            return Ok(false);
        }
        require_success(&output, "look up snapshot")?;
        Ok(true)
    }

    fn latest(&self, prefix: &str, fingerprint: &str) -> Result<Option<String>> {
        let mut command = self.command()?;
        let output = command
            .args(["--json", "object", "list"])
            .arg(self.remote(&format!("{prefix}/{fingerprint}/")))
            .output()
            .context("start snapshot listing")?;
        require_success(&output, "list snapshots")?;
        let listing: Listing =
            serde_json::from_slice(&output.stdout).context("read snapshot storage listing")?;
        let mut objects = listing
            .items
            .into_iter()
            .map(|item| item.key)
            .filter(|key| key.ends_with(".tar"))
            .collect::<Vec<_>>();
        objects.sort();
        Ok(objects.pop())
    }

    fn copy(&self, source: &Path, object: &str) -> Result<()> {
        let mut command = self.command()?;
        run_command(
            command.arg("put").arg(source).arg(self.remote(object)),
            "upload snapshot",
        )
    }

    fn copy_from(&self, object: &str, destination: &Path) -> Result<()> {
        let mut command = self.command()?;
        run_command(
            command.arg("get").arg(self.remote(object)).arg(destination),
            "download snapshot",
        )
    }

    /// Addresses an object in this job's own bucket, the only one its
    /// credentials reach.
    fn remote(&self, object: &str) -> String {
        format!("snapshot/{}/{object}", self.bucket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_snapshot_names_are_unambiguous_and_safe() {
        assert_eq!(
            Snapshot::object(&"a".repeat(64), &"b".repeat(64)),
            format!(
                "{}/{}/{}.tar",
                Snapshot::PREFIX,
                "a".repeat(64),
                "b".repeat(64)
            )
        );
        assert!(validate_fingerprint(&"a".repeat(64)).is_ok());
        assert!(validate_fingerprint("../trusted").is_err());
        assert!(validate_component("audio", "lane").is_ok());
        assert!(validate_component("audio/../trusted", "lane").is_err());
    }

    /// The lock file is the entire key. A lane, a profile or a toolchain that
    /// changed the fingerprint would manufacture a miss for sources that are
    /// byte-identical, and the layer would go cold for no reason.
    /// A restore runs beside a neighbour that is compiling out of the same
    /// cargo home, so it must never rewrite a file that neighbour may have
    /// open. Both tars can say that; they disagree on the spelling, and the
    /// wrong one either fails the extraction or performs it destructively.
    #[test]
    fn a_restore_never_overwrites_what_the_cargo_home_already_holds() {
        let flag = keep_existing().unwrap();
        assert!(matches!(flag, "--skip-old-files" | "--keep-old-files"));
        let archive = NamedTempFile::new().unwrap();
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("kept"), b"original").unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("kept"), b"replacement").unwrap();
        run_command(
            Command::new("tar")
                .args(["--create", "--file"])
                .arg(archive.path())
                .arg("--directory")
                .arg(source.path())
                .arg("kept"),
            "archive",
        )
        .unwrap();
        run_command(
            Command::new("tar")
                .args(["--extract", flag, "--file"])
                .arg(archive.path())
                .arg("--directory")
                .arg(home.path()),
            "extract",
        )
        .unwrap();
        assert_eq!(fs::read(home.path().join("kept")).unwrap(), b"original");
    }

    #[test]
    fn the_source_key_follows_the_lock_file_alone() {
        let first = tempfile::tempdir().expect("a temporary root");
        let second = tempfile::tempdir().expect("a temporary root");
        fs::write(first.path().join("Cargo.lock"), b"version = 4").expect("a lock file");
        fs::write(second.path().join("Cargo.lock"), b"version = 4").expect("a lock file");

        assert_eq!(
            sources_fingerprint(first.path()).expect("a fingerprint"),
            sources_fingerprint(second.path()).expect("a fingerprint"),
            "the same lock in a different checkout names the same sources"
        );

        fs::write(second.path().join("Cargo.lock"), b"version = 5").expect("a lock file");
        assert_ne!(
            sources_fingerprint(first.path()).expect("a fingerprint"),
            sources_fingerprint(second.path()).expect("a fingerprint"),
            "a changed lock must not serve the previous sources"
        );
    }

    #[test]
    fn a_source_layer_is_recognised_by_its_marker() {
        let home = tempfile::tempdir().expect("a temporary cargo home");

        assert_eq!(read_marker(home.path()).expect("a marker read"), None);
        write_marker(home.path(), "fingerprint").expect("a marker write");
        assert_eq!(
            read_marker(home.path()).expect("a marker read"),
            Some("fingerprint".to_owned())
        );
    }

    #[test]
    fn a_snapshot_object_names_the_checksum_its_bytes_must_have() {
        assert_eq!(
            checksum_of("source-snapshots/key/abc.tar").expect("a checksum"),
            "abc"
        );
        assert!(checksum_of("source-snapshots/key/abc").is_err());
    }

    #[test]
    fn target_snapshots_stay_in_the_compiler_cache_scope() {
        let client = Client {
            bucket: "kithara-review".to_owned(),
            endpoint: String::new(),
            environment: BTreeMap::new(),
            program: PathBuf::from("rc"),
            configuration: tempfile::tempdir().expect("private snapshot client configuration"),
        };

        assert_eq!(
            client.remote("target-snapshots/fingerprint/archive.tar"),
            "snapshot/kithara-review/target-snapshots/fingerprint/archive.tar"
        );
    }

    #[test]
    fn a_restored_snapshot_is_not_published_again() {
        assert_eq!(snapshot_to_publish("hit".to_owned(), true), None);
        assert_eq!(
            snapshot_to_publish("miss".to_owned(), false),
            Some("miss".to_owned())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fingerprint_changes_when_xcode_changes() {
        if let Some(root) = env::var_os("KITHARA_XCODE_FINGERPRINT_TEST_ROOT") {
            let root = Path::new(&root);
            let key =
                fingerprint("apple-lint", "cargo", "host", root, &root.join("cargo")).unwrap();
            fs::write(
                env::var_os("KITHARA_XCODE_FINGERPRINT_TEST_OUTPUT").unwrap(),
                key,
            )
            .unwrap();
            return;
        }
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".config")).unwrap();
        fs::write(root.path().join("Cargo.lock"), "lock").unwrap();
        fs::write(root.path().join(".config/ci-pins.toml"), "pins").unwrap();
        let bin = root.path().join("bin");
        crate::testing::install_double(&bin, "xcodebuild");
        let path = env::join_paths(
            std::iter::once(bin).chain(env::split_paths(&env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let keys: Vec<String> = [
            "Xcode 26.6\nBuild version 26G1",
            "Xcode 27.2\nBuild version 27B52",
        ]
        .iter()
        .enumerate()
        .map(|(index, version)| {
            let output = root.path().join(format!("key-{index}"));
            let status = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ci::cache::snapshot::tests::fingerprint_changes_when_xcode_changes",
                ])
                .env("KITHARA_XCODE_FINGERPRINT_TEST_ROOT", root.path())
                .env("KITHARA_XCODE_FINGERPRINT_TEST_OUTPUT", &output)
                .env("KITHARA_TEST_STDOUT", version)
                .env("PATH", &path)
                .status()
                .unwrap();
            assert!(status.success());
            fs::read_to_string(output).unwrap()
        })
        .collect();
        assert_ne!(
            keys[0], keys[1],
            "a target snapshot must not cross an Xcode build change"
        );
    }

    #[test]
    fn fingerprint_changes_when_the_lockfile_changes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".config")).unwrap();
        fs::write(root.path().join("Cargo.lock"), "first").unwrap();
        fs::write(root.path().join(".config/ci-pins.toml"), "pins").unwrap();
        let cargo_home = root.path().join("cargo");
        let first = fingerprint("audio", "test-release", "host", root.path(), &cargo_home).unwrap();
        fs::write(root.path().join("Cargo.lock"), "second").unwrap();
        let second =
            fingerprint("audio", "test-release", "host", root.path(), &cargo_home).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn fingerprint_separates_non_portable_paths() {
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        for root in [&first_root, &second_root] {
            fs::create_dir_all(root.path().join(".config")).unwrap();
            fs::write(root.path().join("Cargo.lock"), "lock").unwrap();
            fs::write(root.path().join(".config/ci-pins.toml"), "pins").unwrap();
        }

        let first = fingerprint(
            "audio",
            "test-release",
            "host",
            first_root.path(),
            Path::new("/cache/review/cargo"),
        )
        .unwrap();
        let second = fingerprint(
            "audio",
            "test-release",
            "host",
            first_root.path(),
            Path::new("/cache/quarantine/cargo"),
        )
        .unwrap();
        let third = fingerprint(
            "audio",
            "test-release",
            "host",
            second_root.path(),
            Path::new("/cache/review/cargo"),
        )
        .unwrap();

        assert_ne!(first, second);
        assert_ne!(first, third);
    }

    #[test]
    fn restore_refuses_a_target_with_build_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        require_target(&target, true).unwrap();
        fs::write(target.join("artifact"), "compiled").unwrap();
        assert!(require_target(&target, true).is_err());
    }

    #[test]
    fn restore_accepts_the_live_job_markers() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        require_target(&target, true).unwrap();
        fs::write(target.join(lease::FILE), "held").unwrap();
        fs::write(target.join(lease::HEARTBEAT), "alive").unwrap();
        fs::write(target.join(consts::SOURCES_RECORD), "live claim").unwrap();

        assert!(require_target(&target, true).is_ok());
    }

    #[test]
    fn a_warm_lane_keeps_its_build_without_contacting_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("artifact"), "warm").unwrap();

        assert_eq!(
            restore_for_lane(
                "lint",
                &target,
                directory.path(),
                directory.path(),
                Path::new("no-client")
            )
            .expect("a warm build needs no remote restore"),
            None
        );
        assert_eq!(fs::read(target.join("artifact")).unwrap(), b"warm");
    }

    #[test]
    fn an_archive_cannot_replace_live_build_ownership() {
        let directory = tempfile::tempdir().unwrap();
        for name in [lease::FILE, lease::HEARTBEAT, consts::SOURCES_RECORD] {
            fs::write(directory.path().join(name), "foreign owner").unwrap();
            let archive = directory.path().join("snapshot.tar");
            let status = Command::new("tar")
                .args(["--create", "--zstd", "--file"])
                .arg(&archive)
                .arg("--directory")
                .arg(directory.path())
                .arg(name)
                .status()
                .unwrap();
            assert!(status.success());
            assert!(verify_archive(&archive).is_err(), "{name}");
        }
    }

    #[test]
    fn an_archive_path_cannot_escape_the_target() {
        assert!(
            Path::new("./debug/libx.rlib")
                .components()
                .all(|component| matches!(component, Component::CurDir | Component::Normal(_)))
        );
        assert!(
            !Path::new("../trusted")
                .components()
                .all(|component| matches!(component, Component::CurDir | Component::Normal(_)))
        );
    }
}

#[cfg(all(test, unix))]
mod client_tests {
    use super::*;
    use crate::testing::install_script;

    /// Speaks `rc` the way the snapshot client relies on: an alias takes a
    /// name, an endpoint and two credentials and nothing more, an object that
    /// is not there exits 5, a refusal exits 4 with its reason on stderr, a
    /// listing names the archives stored under its prefix, and anything `rc`
    /// would not recognise is a usage error.
    fn rc(directory: &Path) -> PathBuf {
        let store = directory.join("store");
        let program = directory.join("rc");
        install_script(
            &program,
            &format!(
                r#"#!/bin/sh
store='{store}'
case "$1" in
alias) [ "$#" -eq 6 ] || {{ echo "alias set takes a name, an endpoint and two credentials: $*" >&2; exit 2; }} ;;
object)
  case "$3" in
  *denied.tar) echo "Access denied: $3" >&2; exit 4 ;;
  *) [ -f "$store/$3" ] || {{ echo "Not found: $3" >&2; exit 5; }} ;;
  esac ;;
--json)
  case "$4" in
  */listed/) printf '%s' '{{"items":[{{"key":"target-snapshots/listed/sub/","is_dir":true}},{{"key":"target-snapshots/listed/b.tar","size_bytes":1,"is_dir":false}},{{"key":"target-snapshots/listed/a.tar","size_bytes":1,"is_dir":false}},{{"key":"target-snapshots/listed/notes.txt","size_bytes":1,"is_dir":false}}],"truncated":false}}' ;;
  *)
    printf '{{"items":['
    separator=''
    for object in "$store/$4"*.tar; do
      [ -f "$object" ] || continue
      printf '%s{{"key":"%s","size_bytes":1,"is_dir":false}}' "$separator" "${{object#"$store"/snapshot/*/}}"
      separator=','
    done
    printf '],"truncated":false}}' ;;
  esac ;;
put) mkdir -p "$store/$(dirname "$3")" && cp "$2" "$store/$3" ;;
get) cp "$store/$2" "$3" ;;
*) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
                store = store.display()
            ),
        );
        program
    }

    fn client(program: PathBuf) -> Client {
        Client {
            bucket: "kithara-review".to_owned(),
            endpoint: "http://cache:9000".to_owned(),
            environment: BTreeMap::from([
                ("AWS_ACCESS_KEY_ID".to_owned(), "key".to_owned()),
                ("AWS_SECRET_ACCESS_KEY".to_owned(), "secret".to_owned()),
            ]),
            program,
            configuration: tempfile::tempdir().expect("private snapshot client configuration"),
        }
    }

    /// A scope's default branch records the dependency sources its own
    /// branches read: both go through the bucket the job's credentials name,
    /// so no scope reaches into another's. The client reads its credentials
    /// from the job's environment, so the body runs in a child that has them.
    #[test]
    fn the_source_layer_travels_through_the_jobs_own_bucket() {
        const CHILD: &str = "KITHARA_SOURCE_LAYER_TEST";
        let Some(directory) = env::var_os(CHILD).map(PathBuf::from) else {
            let directory = tempfile::tempdir().unwrap();
            let output = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ci::cache::snapshot::client_tests::the_source_layer_travels_through_the_jobs_own_bucket",
                    "--nocapture",
                ])
                .env(CHILD, directory.path())
                .env("SCCACHE_BUCKET", "kithara-review")
                .env("SCCACHE_ENDPOINT", "http://cache:9000")
                .env("AWS_ACCESS_KEY_ID", "key")
                .env("AWS_SECRET_ACCESS_KEY", "secret")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let rc = rc(&directory);
        let root = directory.join("checkout");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
        let recorded = directory.join("recorded");
        fs::create_dir_all(recorded.join("registry/cache")).unwrap();
        fs::write(recorded.join("registry/cache/dependency.crate"), b"crate").unwrap();

        publish_sources(&root, &recorded, &rc).expect("the default branch publishes");
        let read = directory.join("read");
        let restored = restore_sources(&root, &read, &rc).expect("a branch restores");

        let Restored::Layer(object) = restored else {
            panic!("the layer the scope published was not found: {restored:?}");
        };
        assert!(
            directory
                .join("store/snapshot/kithara-review")
                .join(&object)
                .is_file(),
            "{object} is not in the job's own bucket"
        );
        assert_eq!(
            fs::read(read.join("registry/cache/dependency.crate")).unwrap(),
            b"crate"
        );
    }

    #[test]
    fn an_evicted_lane_restores_its_outputs_and_keeps_its_live_lease() {
        const CHILD: &str = "KITHARA_TARGET_LAYER_TEST";
        let Some(directory) = env::var_os(CHILD).map(PathBuf::from) else {
            let directory = tempfile::tempdir().unwrap();
            let output = Command::new(env::current_exe().unwrap())
                .args(["--exact", "ci::cache::snapshot::client_tests::an_evicted_lane_restores_its_outputs_and_keeps_its_live_lease", "--nocapture"])
                .env(CHILD, directory.path())
                .env("SCCACHE_BUCKET", "kithara-review")
                .env("SCCACHE_ENDPOINT", "http://cache:9000")
                .env("AWS_ACCESS_KEY_ID", "key")
                .env("AWS_SECRET_ACCESS_KEY", "secret")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let rc = rc(&directory);
        let root = directory.join("checkout");
        fs::create_dir_all(root.join(".config")).unwrap();
        fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
        fs::write(root.join(".config/ci-pins.toml"), "pins").unwrap();
        let target = directory.join("target");
        fs::create_dir_all(&target).unwrap();
        let home = directory.join("cargo");
        fs::write(target.join(consts::SOURCES_RECORD), "initial claim").unwrap();
        let fingerprint = restore_for_lane("lint", &target, &root, &home, &rc)
            .unwrap()
            .expect("an empty store needs a publisher");
        let output = target.join("debug/build/dependency/out/generated.rs");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, "pub const VALUE: u8 = 7;").unwrap();
        filetime::set_file_mtime(
            &output,
            filetime::FileTime::from_unix_time(1_600_000_000, 123_456_789),
        )
        .unwrap();
        let modified = fs::metadata(&output).unwrap().modified().unwrap();
        fs::write(target.join(lease::FILE), "old lease").unwrap();
        fs::write(target.join(lease::HEARTBEAT), "old heartbeat").unwrap();
        publish_for_lane(&target, &fingerprint, &rc).unwrap();

        fs::remove_dir_all(&target).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join(lease::FILE), "live lease").unwrap();
        fs::write(target.join(lease::HEARTBEAT), "live heartbeat").unwrap();
        fs::write(target.join(consts::SOURCES_RECORD), "current claim").unwrap();
        assert_eq!(
            restore_for_lane("lint", &target, &root, &home, &rc).unwrap(),
            None
        );
        assert_eq!(
            fs::read_to_string(&output).unwrap(),
            "pub const VALUE: u8 = 7;"
        );
        assert_eq!(fs::metadata(&output).unwrap().modified().unwrap(), modified);
        assert_eq!(fs::read(target.join(lease::FILE)).unwrap(), b"live lease");
        assert_eq!(
            fs::read(target.join(lease::HEARTBEAT)).unwrap(),
            b"live heartbeat"
        );
        assert_eq!(
            fs::read(target.join(consts::SOURCES_RECORD)).unwrap(),
            b"current claim"
        );
    }

    #[test]
    fn a_snapshot_travels_through_the_store_and_back() {
        let directory = tempfile::tempdir().unwrap();
        let client = client(rc(directory.path()));
        let source = directory.path().join("upload.tar");
        fs::write(&source, b"snapshot bytes").unwrap();
        let object = "target-snapshots/fingerprint/checksum.tar";

        assert!(!client.exists(object).unwrap());
        client.copy(&source, object).unwrap();
        assert!(client.exists(object).unwrap());
        let download = directory.path().join("download.tar");
        client.copy_from(object, &download).unwrap();
        assert_eq!(fs::read(download).unwrap(), b"snapshot bytes");
    }

    /// Reading a refusal as "absent" sends the job on to an upload the store
    /// refuses too, and the reason it gives then names the wrong step.
    #[test]
    fn a_refused_lookup_is_a_failure_not_an_absent_object() {
        let directory = tempfile::tempdir().unwrap();
        let client = client(rc(directory.path()));

        let error = client
            .exists("target-snapshots/fingerprint/denied.tar")
            .expect_err("a refused lookup must not read as absent");
        assert!(error.to_string().contains("Access denied"), "{error}");
    }

    #[test]
    fn the_latest_snapshot_is_the_highest_archive_listed() {
        let directory = tempfile::tempdir().unwrap();
        let client = client(rc(directory.path()));

        assert_eq!(
            client
                .latest(Snapshot::PREFIX, "listed")
                .unwrap()
                .as_deref(),
            Some("target-snapshots/listed/b.tar")
        );
        assert_eq!(client.latest(Snapshot::PREFIX, "unlisted").unwrap(), None);
    }
}
