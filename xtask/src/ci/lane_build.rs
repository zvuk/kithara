//! A lane's build directory is shared by every checkout on the fleet, and
//! cargo judges freshness by mtime. A persistent checkout keeps the mtime of
//! every file a branch switch left alone, so artifacts another checkout built
//! from other sources can read as newer than those files and be reused
//! unbuilt. The directory records which content its artifacts may come from,
//! and a checkout that claims it stamps every file whose content is not the
//! only one recorded, so cargo rebuilds exactly those and reuses the rest.
//! A build the record did not see — a job that never claimed the directory,
//! or one that died before releasing it — leaves artifacts of unknown content,
//! so every file is stamped until a lane succeeds again.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::FileLock;

use crate::consts;

/// Git blob ids per tracked path.
type Sources = BTreeMap<String, BTreeSet<String>>;

/// One job's hold on a lane build directory.
pub(super) struct LaneBuild {
    _lock: FileLock,
    record: PathBuf,
    claimed: Sources,
    tracked: Sources,
}

impl LaneBuild {
    /// Waits out any other job building the same lane, then stamps the
    /// checkout's files the directory may hold artifacts of other content for.
    /// The record keeps that content too until the lane succeeds, so a job
    /// that dies mid-build leaves the next one stamping the same files.
    pub(super) fn claim(project_root: &Path, dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)
            .with_context(|| format!("creating lane build directory {}", dir.display()))?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(consts::LOCK_FILE))
            .with_context(|| format!("opening the lane build lock in {}", dir.display()))?;
        let lock = FileLock::exclusive(lock).context("waiting for the lane build lock")?;
        let tracked = tracked_sources(project_root)?;
        let record = dir.join(consts::SOURCES_FILE);
        let mut recorded = read_sources(&record)?;
        if unseen_build(dir, &record)? {
            for path in tracked.keys() {
                recorded
                    .entry(path.clone())
                    .or_default()
                    .insert(consts::UNKNOWN_BLOB.to_owned());
            }
        }
        let now = SystemTime::now();
        for path in stale_paths(&recorded, &tracked) {
            let file = project_root.join(path);
            File::options()
                .write(true)
                .open(&file)
                .and_then(|file| file.set_modified(now))
                .with_context(|| format!("stamping {}", file.display()))?;
        }
        for (path, blobs) in &tracked {
            recorded
                .entry(path.clone())
                .or_default()
                .extend(blobs.iter().cloned());
        }
        write_sources(&record, &recorded)?;
        Ok(Self {
            _lock: lock,
            record,
            claimed: recorded,
            tracked,
        })
    }

    /// Records the job's builds as seen. A failed lane invalidates every
    /// tracked source because its cached artifacts did not prove trustworthy.
    pub(super) fn settle(&self, succeeded: bool) -> Result<()> {
        if succeeded {
            return write_sources(&self.record, &self.tracked);
        }
        let mut uncertain = self.claimed.clone();
        for path in self.tracked.keys() {
            uncertain
                .entry(path.clone())
                .or_default()
                .insert(consts::UNKNOWN_BLOB.to_owned());
        }
        write_sources(&self.record, &uncertain)
    }
}

/// Whether cargo wrote a unit fingerprint after the record was last written.
fn unseen_build(dir: &Path, record: &Path) -> Result<bool> {
    let seen = match fs::metadata(record) {
        Ok(metadata) => metadata.modified()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", record.display()));
        }
    };
    // `<profile>/.fingerprint` and `<target triple>/<profile>/.fingerprint`.
    let mut fingerprints = Vec::new();
    for profile in subdirectories(dir)? {
        fingerprints.push(profile.join(".fingerprint"));
        for nested in subdirectories(&profile)? {
            fingerprints.push(nested.join(".fingerprint"));
        }
    }
    for fingerprint in fingerprints {
        for unit in subdirectories(&fingerprint)? {
            for file in
                fs::read_dir(&unit).with_context(|| format!("listing {}", unit.display()))?
            {
                if file?.metadata()?.modified()? > seen {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn subdirectories(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("listing {}", dir.display())),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            found.push(entry.path());
        }
    }
    Ok(found)
}

/// Paths whose recorded content is anything but exactly what is checked out.
fn stale_paths<'a>(recorded: &Sources, tracked: &'a Sources) -> Vec<&'a str> {
    tracked
        .iter()
        .filter(|(path, blobs)| recorded.get(*path) != Some(*blobs))
        .map(|(path, _)| path.as_str())
        .collect()
}

fn tracked_sources(project_root: &Path) -> Result<Sources> {
    let output = Command::new("git")
        .current_dir(project_root)
        .args(["ls-files", "--stage", "-z"])
        .output()
        .context("listing the checkout's tracked files")?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(parse_stage(&String::from_utf8_lossy(&output.stdout)))
}

/// `git ls-files --stage -z` entries are `<mode> <blob> <stage>\t<path>`.
/// Symlinks and submodules carry no content cargo reads through them.
fn parse_stage(listed: &str) -> Sources {
    let mut sources = Sources::new();
    for entry in listed.split('\0') {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut meta = meta.split(' ');
        let (Some(mode), Some(blob)) = (meta.next(), meta.next()) else {
            continue;
        };
        if mode == "120000" || mode == "160000" || path.contains('\n') {
            continue;
        }
        sources
            .entry(path.to_owned())
            .or_default()
            .insert(blob.to_owned());
    }
    sources
}

fn read_sources(record: &Path) -> Result<Sources> {
    let text = match fs::read_to_string(record) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Sources::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", record.display()));
        }
    };
    let mut sources = Sources::new();
    for line in text.lines() {
        if let Some((blob, path)) = line.split_once('\t') {
            sources
                .entry(path.to_owned())
                .or_default()
                .insert(blob.to_owned());
        }
    }
    Ok(sources)
}

fn write_sources(record: &Path, sources: &Sources) -> Result<()> {
    let mut text = String::new();
    for (path, blobs) in sources {
        for blob in blobs {
            text.push_str(blob);
            text.push('\t');
            text.push_str(path);
            text.push('\n');
        }
    }
    let partial = record.with_extension("partial");
    fs::write(&partial, text).with_context(|| format!("writing {}", partial.display()))?;
    fs::rename(&partial, record).with_context(|| format!("replacing {}", record.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(entries: &[(&str, &str)]) -> Sources {
        let mut sources = Sources::new();
        for (path, blob) in entries {
            sources
                .entry((*path).to_owned())
                .or_default()
                .insert((*blob).to_owned());
        }
        sources
    }

    #[test]
    fn only_content_the_directory_did_not_build_alone_is_stamped() {
        let recorded = sources(&[
            ("same.rs", "a"),
            ("changed.rs", "b"),
            ("mixed.rs", "c"),
            ("mixed.rs", "d"),
        ]);
        let tracked = sources(&[
            ("same.rs", "a"),
            ("changed.rs", "e"),
            ("mixed.rs", "c"),
            ("new.rs", "f"),
        ]);

        assert_eq!(
            stale_paths(&recorded, &tracked),
            ["changed.rs", "mixed.rs", "new.rs"]
        );
    }

    #[test]
    fn stage_listing_skips_links_and_submodules() {
        let listed = "100644 aaa 0\tsrc/lib.rs\0120000 bbb 0\tlink\0160000 ccc 0\tvendor\0";

        assert_eq!(parse_stage(listed), sources(&[("src/lib.rs", "aaa")]));
    }

    /// The branch that built the lane last must not hand a file's artifacts to
    /// a checkout whose unchanged copy of that file is older than the build.
    #[test]
    fn a_claim_stamps_what_another_branch_built_until_the_lane_succeeds() {
        let checkout = tempfile::tempdir().unwrap();
        let lane = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .current_dir(checkout.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        fs::write(checkout.path().join("lib.rs"), "one").unwrap();
        git(&["add", "lib.rs"]);
        let file = checkout.path().join("lib.rs");
        let old = SystemTime::UNIX_EPOCH;
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        write_sources(
            &lane.path().join(consts::SOURCES_FILE),
            &sources(&[("lib.rs", "other")]),
        )
        .unwrap();

        let claim = LaneBuild::claim(checkout.path(), lane.path()).unwrap();

        assert!(
            fs::metadata(&file).unwrap().modified().unwrap() > old,
            "stamped"
        );
        claim.settle(true).unwrap();
        drop(claim);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let _claim = LaneBuild::claim(checkout.path(), lane.path()).unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().modified().unwrap(),
            old,
            "a settled lane reuses what it built from this content"
        );
    }

    /// A job without the claim may have rebuilt any unit from other content.
    #[test]
    fn a_build_the_record_did_not_see_stamps_everything_until_the_lane_succeeds() {
        let checkout = tempfile::tempdir().unwrap();
        let lane = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .current_dir(checkout.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        let file = checkout.path().join("lib.rs");
        fs::write(&file, "one").unwrap();
        git(&["add", "lib.rs"]);
        let old = SystemTime::UNIX_EPOCH;
        let set_old = |path: &Path| {
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        set_old(&file);
        let record = lane.path().join(consts::SOURCES_FILE);
        write_sources(&record, &tracked_sources(checkout.path()).unwrap()).unwrap();
        set_old(&record);
        let unit = lane.path().join("debug/.fingerprint/lib-0123");
        fs::create_dir_all(&unit).unwrap();
        fs::write(unit.join("lib-lib"), "hash").unwrap();

        let claim = LaneBuild::claim(checkout.path(), lane.path()).unwrap();
        assert!(
            fs::metadata(&file).unwrap().modified().unwrap() > old,
            "stamped"
        );
        claim.settle(false).unwrap();
        drop(claim);

        set_old(&file);
        let claim = LaneBuild::claim(checkout.path(), lane.path()).unwrap();
        assert!(
            fs::metadata(&file).unwrap().modified().unwrap() > old,
            "a failed lane leaves the unseen content recorded"
        );
        claim.settle(true).unwrap();
    }

    #[test]
    fn a_failed_lane_invalidates_every_tracked_source() {
        let checkout = tempfile::tempdir().unwrap();
        let lane = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .current_dir(checkout.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success());
        let file = checkout.path().join("lib.rs");
        fs::write(&file, "one").unwrap();
        let status = Command::new("git")
            .current_dir(checkout.path())
            .args(["add", "lib.rs"])
            .status()
            .unwrap();
        assert!(status.success());

        let claim = LaneBuild::claim(checkout.path(), lane.path()).unwrap();
        claim.settle(false).unwrap();
        drop(claim);
        let old = SystemTime::UNIX_EPOCH;
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let _claim = LaneBuild::claim(checkout.path(), lane.path()).unwrap();
        assert!(
            fs::metadata(file).unwrap().modified().unwrap() > old,
            "a failed lane cannot certify cached artifacts"
        );
    }
}
