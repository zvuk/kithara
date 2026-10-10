//! Cargo never removes a build unit, so a build directory keeps every unit any
//! of its builds ever produced; a dependency update alone builds every member
//! above it again under a new hash. This pass keeps the units some build still
//! asks for and removes the rest.
//!
//! What a build asks for is told by what it built last: the newest instance of
//! each unit - one package's target of one kind with one feature set - plus
//! every instance built within the window before it, since two builds of one
//! lane can each want their own and run together. Everything those reach
//! through the dependencies their fingerprints name stays, however old: Cargo
//! rewrites a unit only when it builds it again, so a unit every build reuses
//! keeps the date of its first build. A unit nothing kept reaches goes, its
//! fingerprint before its outputs, so a pass cut short leaves a unit Cargo
//! builds again rather than one it believes is still there.
//!
//! The fingerprint format is Cargo's own. A pass that cannot read it removes
//! nothing and says so.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::{info, warn};

use crate::consts;

/// The part of a fingerprint this pass reads. Each dependency is
/// `[package id, crate name, public, fingerprint]`.
#[derive(Deserialize)]
struct Fingerprint {
    features: String,
    deps: Vec<(u64, String, bool, u64)>,
}

/// A unit's directory under a profile's `.fingerprint`, `<package>-<hash>`.
struct UnitDir {
    profile: PathBuf,
    package: String,
    hash: String,
}

/// One build of a unit: a fingerprint Cargo wrote in a unit's directory.
struct Build {
    unit: usize,
    kind: String,
    features: String,
    fingerprint: String,
    deps: Vec<String>,
    built: SystemTime,
}

/// Removes from the build directory `dir` the units no build of it asks for
/// any more and the scratch its tests left.
///
/// # Errors
///
/// When a fingerprint cannot be read, before anything is removed.
pub(super) fn collect(dir: &Path, window: Duration) -> Result<()> {
    let started = Instant::now();
    let mut units = Vec::new();
    let mut builds = Vec::new();
    for profile in profiles(dir)? {
        read_profile(&profile, &mut units, &mut builds)?;
    }
    let live = live_units(&units, &builds, window);
    let mut bytes = scratch(dir)?;
    let mut dead: BTreeMap<&Path, BTreeSet<&str>> = BTreeMap::new();
    for (index, unit) in units.iter().enumerate() {
        if !live.contains(&index) {
            bytes = bytes.saturating_add(remove(
                &unit
                    .profile
                    .join(".fingerprint")
                    .join(format!("{}-{}", unit.package, unit.hash)),
            ));
            dead.entry(unit.profile.as_path())
                .or_default()
                .insert(unit.hash.as_str());
        }
    }
    for (profile, hashes) in &dead {
        for (hash, path) in unit_paths(profile)? {
            if hashes.contains(hash.as_str()) {
                bytes = bytes.saturating_add(remove(&path));
            }
        }
    }
    info!(
        "build directory {}: {} of {} units no build asks for removed, {bytes} bytes freed, {:.1} s",
        dir.display(),
        units.len().saturating_sub(live.len()),
        units.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Whether Cargo wrote a unit fingerprint in `dir` after `time`.
///
/// # Errors
///
/// When the directory cannot be listed.
pub(super) fn built_after(dir: &Path, time: SystemTime) -> Result<bool> {
    for profile in profiles(dir)? {
        for unit in entries(&profile.join(".fingerprint"))? {
            for file in entries(&unit)? {
                let written = fs::symlink_metadata(&file)
                    .and_then(|metadata| metadata.modified())
                    .with_context(|| format!("reading {}", file.display()))?;
                if written > time {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Reads every unit of `profile` and the builds Cargo recorded for it.
fn read_profile(profile: &Path, units: &mut Vec<UnitDir>, builds: &mut Vec<Build>) -> Result<()> {
    for directory in entries(&profile.join(".fingerprint"))? {
        let name = file_name(&directory)?;
        let Some((package, hash)) = name
            .rsplit_once('-')
            .filter(|(_, hash)| is_hash(hash, consts::UNIT_HASH_LEN))
        else {
            bail!(unknown(
                &directory,
                "a unit directory named <package>-<16 hex digits>"
            ));
        };
        let unit = units.len();
        units.push(UnitDir {
            profile: profile.to_path_buf(),
            package: package.to_owned(),
            hash: hash.to_owned(),
        });
        for file in entries(&directory)? {
            let kind = file_name(&file)?;
            let record = directory.join(format!("{kind}.json"));
            if kind.ends_with(".json") || !record.is_file() {
                continue;
            }
            builds.push(read_build(unit, kind, &file, &record)?);
        }
    }
    Ok(())
}

fn read_build(unit: usize, kind: &str, file: &Path, record: &Path) -> Result<Build> {
    let fingerprint = fs::read_to_string(file)
        .with_context(|| format!("reading {}", file.display()))?
        .trim()
        .to_owned();
    if !is_hash(&fingerprint, 16) {
        bail!(unknown(file, "a fingerprint of 16 hex digits"));
    }
    let text =
        fs::read_to_string(record).with_context(|| format!("reading {}", record.display()))?;
    let parsed: Fingerprint = serde_json::from_str(&text)
        .with_context(|| unknown(record, "a fingerprint record Cargo writes"))?;
    let built = fs::metadata(file)
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("reading when {} was built", file.display()))?;
    Ok(Build {
        unit,
        kind: kind.to_owned(),
        features: parsed.features,
        fingerprint,
        deps: parsed
            .deps
            .iter()
            .map(|(_, _, _, dep)| fingerprint_hex(*dep))
            .collect(),
        built,
    })
}

/// The units some build still asks for: the newest build of each unit kind,
/// those within `window` of it, and every unit their dependencies reach.
fn live_units(units: &[UnitDir], builds: &[Build], window: Duration) -> BTreeSet<usize> {
    let mut newest: HashMap<Identity<'_>, SystemTime> = HashMap::new();
    for build in builds {
        let latest = newest.entry(identity(units, build)).or_insert(build.built);
        *latest = (*latest).max(build.built);
    }
    let mut by_fingerprint: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, build) in builds.iter().enumerate() {
        by_fingerprint
            .entry(build.fingerprint.as_str())
            .or_default()
            .push(index);
    }
    let mut reached = vec![false; builds.len()];
    let mut pending: Vec<usize> = builds
        .iter()
        .enumerate()
        .filter(|(_, build)| {
            let latest = newest[&identity(units, build)];
            latest
                .checked_sub(window)
                .is_none_or(|cutoff| build.built >= cutoff)
        })
        .map(|(index, _)| index)
        .collect();
    let mut stale_edges = 0_usize;
    while let Some(index) = pending.pop() {
        if std::mem::replace(&mut reached[index], true) {
            continue;
        }
        for dep in &builds[index].deps {
            match by_fingerprint.get(dep.as_str()) {
                Some(found) => pending.extend(found),
                None => stale_edges += 1,
            }
        }
    }
    if stale_edges > 0 {
        info!(
            "{stale_edges} dependency edges name a fingerprint since rebuilt in place; the unit \
             that named it is built again before its next use"
        );
    }
    builds
        .iter()
        .zip(reached)
        .filter(|(_, reached)| *reached)
        .map(|(build, _)| build.unit)
        .collect()
}

/// What one build asks for: a package's target of one kind, with one feature
/// set, in one profile.
type Identity<'a> = (&'a Path, &'a str, &'a str, &'a str);

fn identity<'a>(units: &'a [UnitDir], build: &'a Build) -> Identity<'a> {
    let unit = &units[build.unit];
    (
        unit.profile.as_path(),
        unit.package.as_str(),
        build.kind.as_str(),
        build.features.as_str(),
    )
}

/// Cargo stores a fingerprint as its eight bytes in hex, little end first.
fn fingerprint_hex(fingerprint: u64) -> String {
    fingerprint
        .to_le_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn is_hash(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unknown(path: &Path, expected: &str) -> String {
    format!(
        "unknown cargo layout at {}: expected {expected}; update the build directory garbage \
         pass for this toolchain",
        path.display()
    )
}

fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 name", path.display()))
}

/// Cargo's profile directories: `dir` itself, `<profile>` and
/// `<triple or nested target>/<profile>`, each told by its `.fingerprint`.
fn profiles(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut candidates = vec![dir.to_path_buf()];
    for top in subdirectories(dir)? {
        candidates.extend(subdirectories(&top)?);
        candidates.push(top);
    }
    Ok(candidates
        .into_iter()
        .filter(|candidate| candidate.join(".fingerprint").is_dir())
        .collect())
}

/// `dir` and each directory in it: Cargo writes `tmp` and `cargo-timings` at
/// the top of a target directory, and a lane may nest one target in another.
pub(super) fn targets(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut targets = subdirectories(dir)?;
    targets.push(dir.to_path_buf());
    Ok(targets)
}

/// The files and directories Cargo names after a unit in `profile`, each with
/// that unit's hash: `<name>-<hash>` before any extension. What it names
/// without one, such as an uplifted binary, belongs to no unit.
fn unit_paths(profile: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut paths = Vec::new();
    for kind in ["build", "deps", "examples"] {
        for path in entries(&profile.join(kind))? {
            let hash = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split('.').next())
                .and_then(|stem| stem.rsplit_once('-'))
                .map(|(_, hash)| hash)
                .filter(|hash| is_hash(hash, consts::UNIT_HASH_LEN))
                .map(str::to_owned);
            if let Some(hash) = hash {
                paths.push((hash, path));
            }
        }
    }
    Ok(paths)
}

/// Removes the scratch tests left (`CARGO_TARGET_TMPDIR`): no build reads it
/// back.
fn scratch(dir: &Path) -> Result<u64> {
    let mut bytes = 0_u64;
    for target in targets(dir)? {
        let tmp = target.join("tmp");
        if tmp.is_dir() {
            bytes = bytes.saturating_add(remove(&tmp));
        }
    }
    Ok(bytes)
}

/// Removes a file or a directory tree and returns the bytes it held. What
/// cannot be removed stays and is logged: the pass only saves disk, and a job
/// never fails for it.
fn remove(path: &Path) -> u64 {
    removed(path).unwrap_or_else(|error| {
        warn!("{error:#}; it stays in the build directory");
        0
    })
}

fn removed(path: &Path) -> Result<u64> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if metadata.is_dir() {
        let bytes = size(path)?;
        fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
        return Ok(bytes);
    }
    fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    Ok(metadata.len())
}

fn size(dir: &Path) -> Result<u64> {
    let mut bytes = 0_u64;
    for path in entries(dir)? {
        let metadata =
            fs::symlink_metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        let held = if metadata.is_dir() {
            size(&path)?
        } else {
            metadata.len()
        };
        bytes = bytes.saturating_add(held);
    }
    Ok(bytes)
}

fn subdirectories(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for path in entries(dir)? {
        if fs::symlink_metadata(&path)
            .with_context(|| format!("reading {}", path.display()))?
            .is_dir()
        {
            found.push(path);
        }
    }
    Ok(found)
}

/// The paths in a directory; none when it does not exist.
fn entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("listing {}", dir.display())),
    };
    let mut paths = Vec::new();
    for entry in listing {
        paths.push(
            entry
                .with_context(|| format!("listing {}", dir.display()))?
                .path(),
        );
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    use super::collect;
    use crate::consts;

    /// One build unit the way Cargo lays it out in `profile`: its fingerprint
    /// directory with the hash file it wrote last at `built`, and its library.
    struct Unit<'a> {
        package: &'a str,
        hash: &'a str,
        fingerprint: u64,
        features: &'a str,
        deps: &'a [u64],
        built: SystemTime,
    }

    impl Unit<'_> {
        fn write(&self, profile: &Path) -> Vec<PathBuf> {
            let crate_name = self.package.replace('-', "_");
            let kind = format!("lib-{crate_name}");
            let directory = profile
                .join(".fingerprint")
                .join(format!("{}-{}", self.package, self.hash));
            fs::create_dir_all(&directory).unwrap();
            let deps = self
                .deps
                .iter()
                .map(|dep| format!("[1,\"dep\",false,{dep}]"))
                .collect::<Vec<_>>()
                .join(",");
            fs::write(
                directory.join(format!("{kind}.json")),
                format!(
                    "{{\"rustc\":1,\"features\":{:?},\"declared_features\":\"\",\"target\":2,\
                     \"profile\":3,\"path\":4,\"deps\":[{deps}],\"local\":[],\"rustflags\":[],\
                     \"config\":0,\"compile_kind\":0}}",
                    self.features
                ),
            )
            .unwrap();
            let hash_file = directory.join(&kind);
            fs::write(&hash_file, hex(self.fingerprint)).unwrap();
            fs::File::options()
                .write(true)
                .open(&hash_file)
                .unwrap()
                .set_modified(self.built)
                .unwrap();
            let library = profile
                .join("deps")
                .join(format!("lib{crate_name}-{}.rlib", self.hash));
            fs::create_dir_all(library.parent().unwrap()).unwrap();
            fs::write(&library, "rlib").unwrap();
            vec![directory, library]
        }
    }

    /// Cargo writes a fingerprint as its eight bytes, little end first.
    fn hex(fingerprint: u64) -> String {
        fingerprint
            .to_le_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn at(hours: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + hours * 60 * 60)
    }

    fn exist(paths: &[PathBuf]) -> bool {
        paths.iter().all(|path| path.exists())
    }

    fn gone(paths: &[PathBuf]) -> bool {
        paths.iter().all(|path| !path.exists())
    }

    /// A dependency update builds a member again under a new hash. The old
    /// instance and what only it reached are what no build uses any more.
    #[test]
    fn what_only_an_outbuilt_instance_reaches_goes() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("debug");
        let old_dep = Unit {
            package: "serde",
            hash: "00000000000000a1",
            fingerprint: 11,
            features: "[\"std\"]",
            deps: &[],
            built: at(0),
        }
        .write(&profile);
        let old_member = Unit {
            package: "kithara-stream",
            hash: "00000000000000b1",
            fingerprint: 12,
            features: "[]",
            deps: &[11],
            built: at(0),
        }
        .write(&profile);
        let new_dep = Unit {
            package: "serde",
            hash: "00000000000000a2",
            fingerprint: 21,
            features: "[\"std\"]",
            deps: &[],
            built: at(48),
        }
        .write(&profile);
        let new_member = Unit {
            package: "kithara-stream",
            hash: "00000000000000b2",
            fingerprint: 22,
            features: "[]",
            deps: &[21],
            built: at(48),
        }
        .write(&profile);

        collect(dir.path(), consts::GARBAGE_WINDOW).unwrap();

        assert!(gone(&old_member), "the outbuilt member goes");
        assert!(gone(&old_dep), "a dependency only it reached goes");
        assert!(exist(&new_member) && exist(&new_dep));
    }

    /// Two builds of one lane can each want their own instance; they are
    /// built in the same run, so they are dated within the window.
    #[test]
    fn instances_built_within_the_window_of_the_newest_stay() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("debug");
        let earlier = Unit {
            package: "kithara-stream",
            hash: "00000000000000b1",
            fingerprint: 12,
            features: "[]",
            deps: &[],
            built: at(30),
        }
        .write(&profile);
        let newest = Unit {
            package: "kithara-stream",
            hash: "00000000000000b2",
            fingerprint: 22,
            features: "[]",
            deps: &[],
            built: at(48),
        }
        .write(&profile);

        collect(dir.path(), consts::GARBAGE_WINDOW).unwrap();

        assert!(exist(&earlier) && exist(&newest));
    }

    /// A unit is never rewritten while it stays fresh, so its date says
    /// nothing about its use: one a kept unit depends on stays at any age.
    #[test]
    fn a_dependency_a_kept_unit_reaches_stays_however_old() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("debug");
        let older_version = Unit {
            package: "rand",
            hash: "00000000000000c1",
            fingerprint: 31,
            features: "[]",
            deps: &[],
            built: at(0),
        }
        .write(&profile);
        Unit {
            package: "rand",
            hash: "00000000000000c2",
            fingerprint: 32,
            features: "[]",
            deps: &[],
            built: at(48),
        }
        .write(&profile);
        Unit {
            package: "kithara-stream",
            hash: "00000000000000b2",
            fingerprint: 22,
            features: "[]",
            deps: &[31, 32],
            built: at(48),
        }
        .write(&profile);

        collect(dir.path(), consts::GARBAGE_WINDOW).unwrap();

        assert!(exist(&older_version));
    }

    /// Features are part of what a build asks for: each set has its own
    /// newest instance.
    #[test]
    fn each_feature_set_keeps_its_own_newest_instance() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("debug");
        let flash_off = Unit {
            package: "kithara-stream",
            hash: "00000000000000b1",
            fingerprint: 12,
            features: "[]",
            deps: &[],
            built: at(0),
        }
        .write(&profile);
        let flash = Unit {
            package: "kithara-stream",
            hash: "00000000000000b2",
            fingerprint: 22,
            features: "[\"flash\"]",
            deps: &[],
            built: at(48),
        }
        .write(&profile);

        collect(dir.path(), consts::GARBAGE_WINDOW).unwrap();

        assert!(exist(&flash_off) && exist(&flash));
    }

    /// The format is Cargo's own and may change under a toolchain update; a
    /// pass that cannot read it removes nothing.
    #[test]
    fn an_unknown_layout_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("debug");
        let old = Unit {
            package: "kithara-stream",
            hash: "00000000000000b1",
            fingerprint: 12,
            features: "[]",
            deps: &[],
            built: at(0),
        }
        .write(&profile);
        let new = Unit {
            package: "kithara-stream",
            hash: "00000000000000b2",
            fingerprint: 22,
            features: "[]",
            deps: &[],
            built: at(48),
        }
        .write(&profile);
        fs::write(
            new[0].join("lib-kithara_stream.json"),
            "{\"deps\":\"elsewhere\"}",
        )
        .unwrap();

        let error = collect(dir.path(), consts::GARBAGE_WINDOW).unwrap_err();

        assert!(
            format!("{error:#}").contains("lib-kithara_stream.json"),
            "{error:#}"
        );
        assert!(exist(&old) && exist(&new));
    }

    /// Tests write their scratch under the target; no build reads it back.
    #[test]
    fn test_scratch_goes() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("tmp/run")).unwrap();

        collect(dir.path(), consts::GARBAGE_WINDOW).unwrap();

        assert!(!dir.path().join("tmp").exists());
    }
}
