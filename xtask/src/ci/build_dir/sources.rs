//! A build directory follows one checkout, and Cargo calls a path package's
//! unit fresh while no file it read is newer than the unit's build. The
//! checkout's mtimes say when git last wrote each file, so a branch checked
//! out in between leaves every file it touched newer than every build, though
//! the file holds again what the directory built from: each job built again
//! whatever the previous job's branch differed in.
//!
//! The directory therefore records the content its builds read, file by file,
//! with the mtime a claim gave that content. A claim gives each file whose
//! content the record names that mtime back and every other file the time of
//! the claim, so Cargo builds what changed since this directory's own builds,
//! whatever ran in the checkout between them. A build script that watches a
//! directory reruns by the newest mtime below it, the directories' own
//! included, and git moves a directory whenever it writes a file into it, so
//! a directory reads as new as the newest file it holds, or as the claim when
//! a file the record names has left it. Those mtimes are older than the
//! ones git wrote, so every build directory of a CI checkout claims before it
//! builds: one that did not would judge a file by a time another directory's
//! record gave it. A settled claim gives them back, so the checkout reads as
//! git wrote it to whatever builds there after the job.
//!
//! The record speaks for the directory while its builds read what the claim
//! stamped. A job that never settled its claim may have built from content it
//! wrote itself, and a build after the record was settled went through no
//! claim at all; either leaves artifacts of unknown content, and the next
//! claim stamps every file.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::{ci::environment::ci_in, consts};

/// The checkout's files by path, each named by its git blob id.
type Contents = BTreeMap<String, String>;

/// A file's content and the mtime a claim gave it.
#[derive(Debug)]
struct Stamped {
    blob: String,
    mtime: SystemTime,
}

/// A build directory's hold on the checkout's mtimes, settled when it is
/// dropped - which a job that dies never does.
#[derive(Debug)]
pub(crate) struct Claim {
    checkout: PathBuf,
    record: PathBuf,
    stamped: BTreeMap<String, Stamped>,
    /// Each stamped file's and directory's mtime before the claim and the one
    /// it was given; the first comes back when the claim settles.
    before: BTreeMap<String, (SystemTime, SystemTime)>,
}

/// Stamps `checkout` for a build in `dir` and records what it stamped, held
/// until the claim settles.
///
/// # Errors
///
/// When the checkout cannot be listed, a file cannot be stamped or the
/// record cannot be read or written.
pub(crate) fn claim(checkout: &Path, dir: &Path) -> Result<Claim> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let record = dir.join(consts::SOURCES_RECORD);
    let recorded = trusted(dir, &record)?;
    let now = SystemTime::now();
    let mut stamped = BTreeMap::new();
    let mut before = BTreeMap::new();
    let mut kept = 0_usize;
    let listed = list(checkout)?;
    let gone: Vec<&String> = recorded
        .keys()
        .filter(|path| !listed.contains_key(*path))
        .collect();
    for (path, blob) in listed {
        let at = match recorded.get(&path) {
            Some(entry) if entry.blob == blob => {
                kept += 1;
                entry.mtime
            }
            _ => now,
        };
        let (was, mtime) = stamp(&checkout.join(&path), at)?;
        before.insert(path.clone(), (was, mtime));
        stamped.insert(path, Stamped { blob, mtime });
    }
    for (directory, at) in directory_stamps(&stamped, &gone, now) {
        let stamps = stamp(&checkout.join(&directory), at)?;
        before.insert(directory, stamps);
    }
    write(&record, &stamped, true)?;
    info!(
        "build directory {}: {kept} of {} files keep the stamp it built them with",
        dir.display(),
        stamped.len()
    );
    Ok(Claim {
        checkout: checkout.to_path_buf(),
        record,
        stamped,
        before,
    })
}

/// Claims `dir` when it is a CI build directory in a build root, beside the
/// alias the root's lanes build through: only there does every build
/// directory of the checkout claim. `var` reads the job's environment.
///
/// # Errors
///
/// When the claim fails.
pub(crate) fn claim_beside_alias(
    checkout: &Path,
    dir: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Option<Claim>> {
    let beside = dir
        .parent()
        .is_some_and(|root| root.join(consts::BUILD_ALIAS).is_symlink());
    if !(beside && ci_in(var)) {
        return Ok(None);
    }
    claim(checkout, dir).map(Some)
}

impl Drop for Claim {
    /// Settles the claim. A file whose mtime moved was written during the
    /// job, so what was built from it read content no claim stamped, and the
    /// record lets it go.
    ///
    /// Every other file, and every directory the job left as stamped, gets
    /// back the mtime it had before the claim. A build
    /// that claims nothing judges the checkout by when git wrote each file,
    /// and a stamp older than that write would call a build of other content
    /// fresh.
    fn drop(&mut self) {
        let checkout = &self.checkout;
        let untouched = |path: &str, given: SystemTime| {
            fs::metadata(checkout.join(path))
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|mtime| mtime == given)
        };
        self.stamped
            .retain(|path, entry| untouched(path, entry.mtime));
        if let Err(error) = write(&self.record, &self.stamped, false) {
            warn!("{error:#}; the next claim of this build directory stamps every file");
        }
        for (path, &(was, given)) in &self.before {
            if untouched(path, given)
                && let Err(error) = stamp(&checkout.join(path), was)
            {
                warn!("{error:#}; a build that claims nothing may trust its stamp");
            }
        }
    }
}

/// What the record says the directory's builds read, while it speaks for
/// them; nothing once it does not.
fn trusted(dir: &Path, record: &Path) -> Result<BTreeMap<String, Stamped>> {
    let text = match fs::read_to_string(record) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", record.display()));
        }
    };
    let (settled, recorded) =
        parse(&text).with_context(|| format!("reading {}", record.display()))?;
    if !settled {
        info!(
            "build directory {}: no job settled its last claim, so every file is stamped",
            dir.display()
        );
        return Ok(BTreeMap::new());
    }
    let written = fs::metadata(record)
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("reading {}", record.display()))?;
    if super::garbage::built_after(dir, written)? {
        info!(
            "build directory {}: built in after its record was settled, so every file is stamped",
            dir.display()
        );
        return Ok(BTreeMap::new());
    }
    Ok(recorded)
}

/// Each directory holding a listed file, with the stamp it reads as: the
/// newest of the files below it, or `now` once a file in `gone` left it.
fn directory_stamps(
    stamped: &BTreeMap<String, Stamped>,
    gone: &[&String],
    now: SystemTime,
) -> BTreeMap<String, SystemTime> {
    let mut directories = BTreeMap::new();
    for (path, entry) in stamped {
        for directory in directories_of(path) {
            let newest = directories.entry(directory).or_insert(entry.mtime);
            *newest = (*newest).max(entry.mtime);
        }
    }
    for path in gone {
        for directory in directories_of(path) {
            if let Some(at) = directories.get_mut(&directory) {
                *at = now;
            }
        }
    }
    directories
}

/// The directories `path` lies in, the checkout's own included.
fn directories_of(path: &str) -> impl Iterator<Item = String> + '_ {
    Path::new(path)
        .ancestors()
        .skip(1)
        .map(|directory| directory.to_string_lossy().into_owned())
}

/// Gives `path` the mtime `at` unless it reads so already, and returns the
/// mtime it had and the one the file system kept, which may be coarser.
fn stamp(path: &Path, at: SystemTime) -> Result<(SystemTime, SystemTime)> {
    let modified = || {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("reading {}", path.display()))
    };
    let mtime = modified()?;
    if mtime == at {
        return Ok((mtime, at));
    }
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(at))
        .with_context(|| format!("stamping {}", path.display()))?;
    Ok((mtime, modified()?))
}

/// The record's header, `held` or `settled`, then one
/// `<blob>\t<seconds>\t<nanoseconds>\t<path>` line per file.
fn parse(text: &str) -> Result<(bool, BTreeMap<String, Stamped>)> {
    let mut lines = text.split('\n').filter(|line| !line.is_empty());
    let settled = match lines.next() {
        Some(consts::SOURCES_SETTLED) => true,
        Some(consts::SOURCES_HELD) => false,
        header => bail!(
            "a record opens with `{}` or `{}`, not {header:?}",
            consts::SOURCES_SETTLED,
            consts::SOURCES_HELD
        ),
    };
    let mut recorded = BTreeMap::new();
    for line in lines {
        let mut fields = line.splitn(4, '\t');
        let (Some(blob), Some(seconds), Some(nanos), Some(path)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            bail!("a record line is `<blob>\\t<seconds>\\t<nanoseconds>\\t<path>`, not {line:?}");
        };
        let since = Duration::new(
            seconds
                .parse()
                .with_context(|| format!("seconds in {line:?}"))?,
            nanos
                .parse()
                .with_context(|| format!("nanoseconds in {line:?}"))?,
        );
        recorded.insert(
            path.to_owned(),
            Stamped {
                blob: blob.to_owned(),
                mtime: SystemTime::UNIX_EPOCH + since,
            },
        );
    }
    Ok((settled, recorded))
}

fn write(record: &Path, stamped: &BTreeMap<String, Stamped>, held: bool) -> Result<()> {
    let header = if held {
        consts::SOURCES_HELD
    } else {
        consts::SOURCES_SETTLED
    };
    let mut lines = vec![header.to_owned()];
    for (path, entry) in stamped {
        let since = entry
            .mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .with_context(|| format!("{path} is stamped before 1970"))?;
        lines.push(format!(
            "{}\t{}\t{}\t{path}",
            entry.blob,
            since.as_secs(),
            since.subsec_nanos()
        ));
    }
    lines.push(String::new());
    let partial = record.with_extension("partial");
    fs::write(&partial, lines.join("\n"))
        .with_context(|| format!("writing {}", partial.display()))?;
    fs::rename(&partial, record).with_context(|| format!("replacing {}", record.display()))
}

/// The checkout's files as the worktree holds them, which is what Cargo
/// reads: the index, with every unstaged edit, deletion and untracked file
/// git does not ignore applied.
fn list(checkout: &Path) -> Result<Contents> {
    let mut contents = parse_stage(&git(checkout, &["ls-files", "--stage", "-z"])?);
    let changed = git(
        checkout,
        &[
            "ls-files",
            "--modified",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let mut present = Vec::new();
    for path in changed.split('\0').filter(|path| !path.is_empty()) {
        contents.remove(path);
        let is_file =
            fs::symlink_metadata(checkout.join(path)).is_ok_and(|metadata| metadata.is_file());
        if is_file && !path.contains('\n') && !present.contains(&path) {
            present.push(path);
        }
    }
    if present.is_empty() {
        return Ok(contents);
    }
    let mut args = vec!["hash-object", "--"];
    args.extend(present.iter());
    let blobs = git(checkout, &args)?;
    for (path, blob) in present.into_iter().zip(blobs.lines()) {
        contents.insert(path.to_owned(), blob.to_owned());
    }
    Ok(contents)
}

/// Runs git in the checkout and returns what it printed. A hook or a caller
/// may point git at another repository through its environment, so those
/// variables are dropped and git names this checkout alone.
fn git(checkout: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(checkout)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} in {} failed: {}",
            args.join(" "),
            checkout.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `git ls-files --stage -z` entries are `<mode> <blob> <stage>\t<path>`.
/// Links and submodules carry no content Cargo reads through them. A path
/// with conflicting stages is named by all of its blobs.
fn parse_stage(listed: &str) -> Contents {
    let mut contents = Contents::new();
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
        contents
            .entry(path.to_owned())
            .and_modify(|blobs| {
                blobs.push(',');
                blobs.push_str(blob);
            })
            .or_insert_with(|| blob.to_owned());
    }
    contents
}

#[cfg(test)]
mod tests {
    use std::{fs, mem::ManuallyDrop, path::Path, time::SystemTime};

    use super::*;
    use crate::{
        ci::build_dir::fixture::{cargo_build, git, git_checkout},
        consts,
    };

    fn set_mtime(path: &Path, at: SystemTime) {
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(at)).unwrap();
    }

    fn mtime(path: &Path) -> SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    /// Whether Cargo compiled the probe package when asked to build the
    /// checkout into `dir`.
    fn compiles(checkout: &Path, dir: &Path) -> bool {
        let output = cargo_build(checkout, dir);
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stdout).lines().any(|line| {
            line.contains("\"reason\":\"compiler-artifact\"") && line.contains("\"fresh\":false")
        })
    }

    /// What claims are for: Cargo builds a package again only when its
    /// content changed since this directory built it, though git wrote the
    /// file again in between.
    #[test]
    fn cargo_reuses_what_the_directory_built_from_the_same_content() {
        let checkout = git_checkout(&[
            ("Cargo.toml", consts::PROBE_MANIFEST),
            ("src/lib.rs", "pub fn probe() {}\n"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let lib = checkout.path().join("src/lib.rs");
        {
            let _claim = claim(checkout.path(), dir.path()).unwrap();
            assert!(
                compiles(checkout.path(), dir.path()),
                "a first build compiles"
            );
        }
        // Another branch came and went: git wrote the file again, unchanged.
        set_mtime(&lib, SystemTime::now() + consts::DAY);

        let _claim = claim(checkout.path(), dir.path()).unwrap();

        assert!(
            !compiles(checkout.path(), dir.path()),
            "content the directory built from is not built again"
        );
    }

    /// A build script that watches a directory reruns when anything below it
    /// is newer than its last run, the directories included, and git moves a
    /// directory whenever it writes a file into it.
    #[test]
    fn cargo_reuses_a_build_script_whose_directory_holds_what_it_built_from() {
        let checkout = git_checkout(&[
            ("Cargo.toml", consts::PROBE_MANIFEST),
            (
                "build.rs",
                "fn main() { println!(\"cargo::rerun-if-changed=assets\"); }\n",
            ),
            ("src/lib.rs", "pub fn probe() {}\n"),
            ("assets/skin/dark.ron", "()\n"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let skin = checkout.path().join("assets/skin");
        let dark = skin.join("dark.ron");
        {
            let _claim = claim(checkout.path(), dir.path()).unwrap();
            assert!(
                compiles(checkout.path(), dir.path()),
                "a first build compiles"
            );
        }
        // Another branch came and went: git wrote the file again, unchanged,
        // and its directory with it.
        fs::remove_file(&dark).unwrap();
        fs::write(&dark, "()\n").unwrap();
        let rewritten = SystemTime::now() + consts::DAY;
        for path in [checkout.path().join("assets"), skin, dark] {
            set_mtime(&path, rewritten);
        }

        let _claim = claim(checkout.path(), dir.path()).unwrap();

        assert!(
            !compiles(checkout.path(), dir.path()),
            "a directory holding what the build read does not run its build script again"
        );
    }

    /// A file the directory built from left a directory, which only the
    /// directory's own mtime says.
    #[test]
    fn a_directory_a_built_file_left_is_stamped_when_it_is_claimed() {
        let checkout = git_checkout(&[("assets/kept.ron", "same"), ("assets/gone.ron", "old")]);
        let dir = tempfile::tempdir().unwrap();
        let assets = checkout.path().join("assets");
        drop(claim(checkout.path(), dir.path()).unwrap());
        fs::remove_file(assets.join("gone.ron")).unwrap();
        set_mtime(&assets, SystemTime::UNIX_EPOCH + consts::DAY);
        let claimed = SystemTime::now();

        let _claim = claim(checkout.path(), dir.path()).unwrap();

        assert!(mtime(&assets) >= claimed);
        assert!(mtime(&assets.join("kept.ron")) < claimed);
    }

    /// A branch checked out in between writes a file again with the content
    /// the directory built from: the claim gives it back the mtime that
    /// content was stamped with, and stamps what the directory never built
    /// from with the time of the claim.
    #[test]
    fn a_claim_gives_content_the_directory_built_from_its_stamp_back() {
        let checkout = git_checkout(&[("kept.rs", "same"), ("changed.rs", "before")]);
        let dir = tempfile::tempdir().unwrap();
        let kept = checkout.path().join("kept.rs");
        let changed = checkout.path().join("changed.rs");
        let first = claim(checkout.path(), dir.path()).unwrap();
        let built_from = mtime(&kept);
        drop(first);
        let rewritten = built_from + consts::DAY;
        fs::write(&changed, "after").unwrap();
        set_mtime(&kept, rewritten);
        set_mtime(&changed, rewritten);
        let claimed = SystemTime::now();

        let _claim = claim(checkout.path(), dir.path()).unwrap();

        assert_eq!(mtime(&kept), built_from);
        assert!(
            (claimed..rewritten).contains(&mtime(&changed)),
            "other content is stamped when it is claimed"
        );
    }

    /// A settled claim gives each file the job left as it stamped it the
    /// mtime it had before. A build that claims nothing, `production/main`'s
    /// among them on a host that serves both, judges the checkout by when git
    /// wrote each file, and a stamp older than that write calls a build of
    /// other content fresh. A file the job wrote keeps the mtime it wrote.
    #[test]
    fn a_settled_claim_gives_the_checkout_its_mtimes_back() {
        let checkout = git_checkout(&[("src/kept.rs", "same"), ("written.rs", "before")]);
        let dir = tempfile::tempdir().unwrap();
        let src = checkout.path().join("src");
        let kept = src.join("kept.rs");
        let written = checkout.path().join("written.rs");
        drop(claim(checkout.path(), dir.path()).unwrap());
        let git_wrote = mtime(&kept) + consts::DAY;
        for path in [&src, &kept, &written] {
            set_mtime(path, git_wrote);
        }
        let job = claim(checkout.path(), dir.path()).unwrap();
        assert_ne!(
            (mtime(&kept), mtime(&src)),
            (git_wrote, git_wrote),
            "the claim gives built content and its directory their stamp"
        );
        fs::write(&written, "after").unwrap();
        let job_wrote = mtime(&written);

        drop(job);

        assert_eq!(mtime(&kept), git_wrote);
        assert_eq!(mtime(&src), git_wrote);
        assert_eq!(mtime(&written), job_wrote);
    }

    /// The job wrote a file and put its content back: what it built in
    /// between read other content, so the file is no longer the record's.
    #[test]
    fn a_file_written_during_the_job_is_stamped_at_the_next_claim() {
        let checkout = git_checkout(&[("lib.rs", "one")]);
        let dir = tempfile::tempdir().unwrap();
        let file = checkout.path().join("lib.rs");
        let job = claim(checkout.path(), dir.path()).unwrap();
        fs::write(&file, "two").unwrap();
        fs::write(&file, "one").unwrap();
        set_mtime(&file, mtime(&file) + consts::DAY);
        drop(job);
        set_mtime(&file, SystemTime::UNIX_EPOCH + consts::DAY);
        let claimed = SystemTime::now();

        let _claim = claim(checkout.path(), dir.path()).unwrap();

        assert!(mtime(&file) >= claimed);
    }

    /// A job that died never settled its claim, and may have written what it
    /// built from: nothing in the record is trusted.
    #[test]
    fn after_a_claim_no_job_settled_every_file_is_stamped() {
        let checkout = git_checkout(&[("lib.rs", "one")]);
        let dir = tempfile::tempdir().unwrap();
        let file = checkout.path().join("lib.rs");
        let _died = ManuallyDrop::new(claim(checkout.path(), dir.path()).unwrap());
        set_mtime(&file, SystemTime::UNIX_EPOCH + consts::DAY);
        let claimed = SystemTime::now();

        let _claim = claim(checkout.path(), dir.path()).unwrap();

        assert!(mtime(&file) >= claimed);
    }

    /// A build that went through no claim read whatever the checkout held,
    /// so the record no longer speaks for the directory.
    #[test]
    fn after_a_build_the_record_did_not_see_every_file_is_stamped() {
        let checkout = git_checkout(&[("lib.rs", "one")]);
        let dir = tempfile::tempdir().unwrap();
        let file = checkout.path().join("lib.rs");
        drop(claim(checkout.path(), dir.path()).unwrap());
        let unit = dir.path().join("debug/.fingerprint/lib-0123456789abcdef");
        fs::create_dir_all(&unit).unwrap();
        fs::write(unit.join("lib-lib"), "0123456789abcdef").unwrap();
        set_mtime(&unit.join("lib-lib"), SystemTime::now() + consts::DAY);
        set_mtime(&file, SystemTime::UNIX_EPOCH + consts::DAY);
        let claimed = SystemTime::now();

        let _claim = claim(checkout.path(), dir.path()).unwrap();

        assert!(mtime(&file) >= claimed);
    }

    /// Only a CI build directory beside its root's alias claims: everywhere
    /// else a build that never claims may share the checkout.
    #[cfg(unix)]
    #[test]
    fn only_a_ci_build_directory_beside_the_alias_claims() {
        let checkout = git_checkout(&[("lib.rs", "one")]);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("network");
        let ci = |name: &str| (name == "CI").then(|| OsString::from("true"));
        let elsewhere = |_: &str| None;

        assert!(
            claim_beside_alias(checkout.path(), &dir, &ci)
                .unwrap()
                .is_none(),
            "no alias beside it"
        );
        std::os::unix::fs::symlink("lint", root.path().join(consts::BUILD_ALIAS)).unwrap();
        assert!(
            claim_beside_alias(checkout.path(), &dir, &elsewhere)
                .unwrap()
                .is_none(),
            "outside a CI job"
        );
        assert!(
            claim_beside_alias(checkout.path(), &dir.join("flash-off"), &ci)
                .unwrap()
                .is_none(),
            "nested in a build directory, whose own claim covers it"
        );
        assert!(
            claim_beside_alias(checkout.path(), &dir, &ci)
                .unwrap()
                .is_some()
        );
    }

    /// Cargo reads the worktree, not the index: an unstaged edit, a deleted
    /// file and an untracked one are named as the worktree holds them, as if
    /// every change were staged, and an ignored file is not named at all.
    #[test]
    fn the_listing_names_what_the_worktree_holds() {
        let checkout = git_checkout(&[
            ("edited.rs", "before"),
            ("deleted.rs", "gone"),
            ("kept.rs", "same"),
        ]);
        let root = checkout.path();
        fs::write(root.join("edited.rs"), "after").unwrap();
        fs::remove_file(root.join("deleted.rs")).unwrap();
        fs::write(root.join("untracked.rs"), "new").unwrap();
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        fs::write(root.join("ignored.log"), "noise").unwrap();

        let listed = list(root).unwrap();
        git(root, &["add", "-A"]);

        assert_eq!(listed, list(root).unwrap());
        assert!(listed.contains_key("untracked.rs"));
        assert!(!listed.contains_key("deleted.rs"));
        assert!(!listed.contains_key("ignored.log"));
    }

    #[test]
    fn the_stage_listing_skips_links_and_submodules() {
        let listed = "100644 aaa 0\tsrc/lib.rs\x00120000 bbb 0\tlink\x00160000 ccc 0\tvendor\0";

        assert_eq!(
            parse_stage(listed),
            Contents::from([("src/lib.rs".to_owned(), "aaa".to_owned())])
        );
    }
}
