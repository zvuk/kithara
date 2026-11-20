use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::{
        fd::AsFd,
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::MetadataExt,
        },
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use nix::{
    dir::Dir,
    errno::Errno,
    fcntl,
    sys::stat::{self, Mode},
};
use tempfile::{Builder, TempDir};
use tracing::{debug, info, warn};

use super::announce;
use crate::{config::LaneReport, consts::LANE_REPORT_ENV};

pub(in crate::ci::lane) mod consts {
    use nix::fcntl::OFlag;

    /// The `chromedriver` log in a Chromium report.
    pub(in crate::ci::lane) const CHROMIUM_DRIVER_LOG: &str = "web-chromium.log";
    /// The wrapper's network log directory in a Chromium report.
    pub(in crate::ci::lane) const CHROMIUM_NETLOG_DIR: &str = "web-chromium-netlog";
    /// Where the test wrapper writes its network logs; without it, it writes none.
    pub(in crate::ci::lane) const CHROMIUM_NETLOG_ENV: &str = "KITHARA_WEB_NETLOG_DIR";
    /// How every capture and every job copy of a Chromium report is named.
    pub(super) const REPORT_PREFIX: &str = "web-chromium-report-";

    /// A directory, opened without following a link to it.
    pub(super) const DIRECTORY: OFlag = OFlag::O_RDONLY
        .union(OFlag::O_DIRECTORY)
        .union(OFlag::O_NOFOLLOW)
        .union(OFlag::O_CLOEXEC);
    /// A file in a report, opened without following a link to it, and without
    /// waiting when it is a pipe rather than a file.
    pub(super) const LEAF: OFlag = OFlag::O_RDONLY
        .union(OFlag::O_NOFOLLOW)
        .union(OFlag::O_NONBLOCK)
        .union(OFlag::O_CLOEXEC);
}

/// One run's Chromium report, captured in a fresh directory: in a GitHub job
/// under the job's own `RUNNER_TEMP`, outside the build slot other jobs and
/// their writers reuse; anywhere else in the build the run holds, as a local
/// diagnostic.
pub(super) struct ChromiumReport {
    capture: PathBuf,
    directories: Directories,
    destination: Destination,
}

/// The capture and network log directories, held open from their creation.
/// What the report copies is read through these, so a name pointed elsewhere
/// during the run cannot make another directory this run's report.
///
/// Each was empty when it was opened. That says what they held then, not who
/// wrote what they hold later: the report is whatever reached them through
/// its steps' paths, from any writer that can reach those paths. Nothing here
/// removes anything from either; a capture stays where it was made.
struct Directories {
    capture: File,
    netlog: File,
}

/// Where a wanted report goes once the lane ends.
enum Destination {
    /// A GitHub job: a copy in the job's own `RUNNER_TEMP`, which no other
    /// job taking the released build can reach, told to later steps only once
    /// every file is copied.
    Job { copy: TempDir, env_file: PathBuf },
    /// Anywhere else: the capture stays in the build and the log names it.
    InPlace,
}

impl ChromiumReport {
    /// Makes the capture directory and, in a GitHub job, the job's copy, both
    /// in the job's `RUNNER_TEMP`; anywhere else the capture is made in
    /// `build`. A GitHub job without its storage fails here, before the
    /// browser runs, rather than running without a report.
    pub(super) fn prepare(
        build: &Path,
        github: bool,
        var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        fs::create_dir_all(build).with_context(|| format!("creating {}", build.display()))?;
        let build =
            fs::canonicalize(build).with_context(|| format!("resolving {}", build.display()))?;
        // A GitHub job captures in its own RUNNER_TEMP, outside the build slot
        // later jobs reuse; anywhere else the capture stays in the build, a
        // local diagnostic.
        let (home, destination) = if github {
            let (Some(env_file), Some(temp)) = (var("GITHUB_ENV"), var("RUNNER_TEMP")) else {
                bail!("a GitHub job names GITHUB_ENV and RUNNER_TEMP");
            };
            let temp = fs::canonicalize(&temp)
                .with_context(|| format!("resolving RUNNER_TEMP {}", Path::new(&temp).display()))?;
            let builds = build.parent().unwrap_or(&build);
            ensure!(
                !temp.starts_with(builds),
                "RUNNER_TEMP {} is inside the builds other jobs take, {}",
                temp.display(),
                builds.display()
            );
            // The name ends as the report does, which is how a reader tells
            // this whole path from a fragment a short write left behind.
            let copy = Builder::new()
                .prefix(consts::REPORT_PREFIX)
                .suffix(&format!(".{}", LaneReport::Chromium.name()))
                .tempdir_in(&temp)
                .with_context(|| format!("creating this job's copy in {}", temp.display()))?;
            let destination = Destination::Job {
                copy,
                env_file: PathBuf::from(env_file),
            };
            (temp, destination)
        } else {
            (build, Destination::InPlace)
        };
        let parent = open_directory(fcntl::AT_FDCWD, &home)
            .with_context(|| format!("opening {}", home.display()))?;
        // Made and opened relative to its parent's descriptor, and nothing
        // removes it by name: a run that ends without its report leaves it.
        let (held, capture) = Builder::new()
            .prefix(consts::REPORT_PREFIX)
            .make_in(&home, |path| {
                let name = Path::new(path.file_name().unwrap_or(path.as_os_str()));
                stat::mkdirat(&parent, name, Mode::S_IRWXU)?;
                Ok(open_directory(&parent, name)?)
            })
            .and_then(|made| made.keep().map_err(|error| error.error))
            .with_context(|| format!("creating a capture directory in {}", home.display()))?;
        ensure!(
            capture
                .to_str()
                .is_some_and(|path| !path.contains(char::is_whitespace)),
            "chromedriver cannot take {} as one argument",
            capture.display()
        );
        // What was opened is what was made only while nothing is in it yet.
        ensure!(
            names_in(&held)?.is_empty(),
            "{} held something before this run wrote to it",
            capture.display()
        );
        stat::mkdirat(&held, consts::CHROMIUM_NETLOG_DIR, Mode::S_IRWXU).with_context(|| {
            format!(
                "creating the network log directory in {}",
                capture.display()
            )
        })?;
        let netlog =
            open_directory(&held, Path::new(consts::CHROMIUM_NETLOG_DIR)).with_context(|| {
                format!("opening the network log directory in {}", capture.display())
            })?;
        ensure!(
            names_in(&netlog)?.is_empty(),
            "the network log directory in {} held something before this run wrote to it",
            capture.display()
        );
        Ok(Self {
            capture,
            directories: Directories {
                capture: held,
                netlog,
            },
            destination,
        })
    }

    /// What the lane's steps are told: the `chromedriver` log path and the
    /// wrapper's network log directory.
    pub(super) fn env(&self) -> [(OsString, OsString); 2] {
        let mut driver = OsString::from("--verbose --log-path=");
        driver.push(self.capture.join(consts::CHROMIUM_DRIVER_LOG));
        [
            (OsString::from("CHROMEDRIVER_ARGS"), driver),
            (
                OsString::from(consts::CHROMIUM_NETLOG_ENV),
                self.capture
                    .join(consts::CHROMIUM_NETLOG_DIR)
                    .into_os_string(),
            ),
        ]
    }

    /// Hands a wanted report over before the build is released. A copy that
    /// stopped part way, or lacks the driver's log, is not told, so the job's
    /// check fails beside the lane's own result instead of uploading part of
    /// a report. Each file holds what its writer had written when it was
    /// opened; nothing here judges whether a network log ended.
    pub(super) fn hand_over(self) {
        let Some(report) = self.still_named() else {
            return;
        };
        match report.destination {
            Destination::Job { copy, env_file } => {
                let handed = report.directories.copy_into(copy.path()).and_then(|()| {
                    announce(&env_file, LANE_REPORT_ENV, copy.path())
                        .with_context(|| format!("writing {}", env_file.display()))
                });
                match handed {
                    Ok(()) => {
                        let _ = copy.keep();
                    }
                    Err(error) => warn!("this job's Chromium report is not uploaded: {error:#}"),
                }
            }
            Destination::InPlace => {
                info!(
                    "this run's Chromium report is in {}",
                    report.capture.display()
                );
            }
        }
    }

    /// Leaves a report nobody wants where it was captured, neither told nor
    /// copied.
    pub(super) fn discard(self) {
        debug!(
            "this run's unwanted Chromium report stays in {}",
            self.capture.display()
        );
    }

    /// This report, while its capture directory still names the directories
    /// the steps were given. Otherwise it is left as it stands: whatever took
    /// their place is not this run's to upload.
    fn still_named(self) -> Option<Self> {
        let problem = match self.directories.named_by(&self.capture) {
            Ok(true) => return Some(self),
            Ok(false) => "no longer names the directories this run's steps were given".to_owned(),
            Err(error) => {
                format!(
                    "cannot be checked against the directories this run's steps were given: {error}"
                )
            }
        };
        warn!(
            "{} {problem}; nothing in it is uploaded",
            self.capture.display()
        );
        None
    }
}

impl Directories {
    /// Whether `capture` still names these directories, without following a
    /// link to either.
    fn named_by(&self, capture: &Path) -> io::Result<bool> {
        Ok(is_named(&self.capture, capture)?
            && is_named(&self.netlog, &capture.join(consts::CHROMIUM_NETLOG_DIR))?)
    }

    /// Copies the driver's log, which every report has, and each network log
    /// the wrapper wrote, each read through the directory it was written in.
    fn copy_into(&self, copy: &Path) -> Result<()> {
        let driver = copy_leaf(
            &self.capture,
            OsStr::new(consts::CHROMIUM_DRIVER_LOG),
            &copy.join(consts::CHROMIUM_DRIVER_LOG),
        )?;
        ensure!(
            driver.is_some_and(|bytes| bytes > 0),
            "chromedriver left no log of its own"
        );
        let netlog_copy = copy.join(consts::CHROMIUM_NETLOG_DIR);
        fs::create_dir(&netlog_copy)
            .with_context(|| format!("creating {}", netlog_copy.display()))?;
        let mut listing = Dir::openat(&self.netlog, ".", consts::DIRECTORY, Mode::empty())
            .context("listing the network log directory")?;
        for entry in listing.iter() {
            let entry = entry.context("listing the network log directory")?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if Path::new(name).extension() == Some(OsStr::new("jsonl")) {
                copy_leaf(&self.netlog, name, &netlog_copy.join(name))?;
            }
        }
        Ok(())
    }
}

/// The names in `dir`, other than itself and its parent.
fn names_in(dir: &File) -> Result<Vec<OsString>> {
    let mut listing = Dir::openat(dir, ".", consts::DIRECTORY, Mode::empty())
        .context("listing a capture directory")?;
    let mut names = Vec::new();
    for entry in listing.iter() {
        let entry = entry.context("listing a capture directory")?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
    Ok(names)
}

/// Opens the directory at `path`, relative to `parent`.
fn open_directory(parent: impl AsFd, path: &Path) -> nix::Result<File> {
    fcntl::openat(parent, path, consts::DIRECTORY, Mode::empty()).map(File::from)
}

/// Whether `path`, not followed when it is a link, is the object `held` is.
fn is_named(held: &File, path: &Path) -> io::Result<bool> {
    let named = match path.symlink_metadata() {
        Ok(named) => named,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let held = held.metadata()?;
    Ok(named.dev() == held.dev() && named.ino() == held.ino())
}

/// Copies `name` in `dir` to `destination`, up to the length it had when it
/// was opened, and says how many bytes that was. A name that is missing, a
/// link, not a regular file, a file with another name, or empty is not
/// copied; what is copied is not thereby shown to be this run's own.
fn copy_leaf(dir: &File, name: &OsStr, destination: &Path) -> Result<Option<u64>> {
    let file = match fcntl::openat(dir, name, consts::LEAF, Mode::empty()) {
        Ok(file) => File::from(file),
        Err(Errno::ELOOP | Errno::ENOENT) => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("opening {}", Path::new(name).display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("reading {}", Path::new(name).display()))?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() == 0 {
        return Ok(None);
    }
    copy_opened(&file, metadata.len(), destination)
        .with_context(|| format!("copying {}", Path::new(name).display()))
}

/// Copies at most `len` bytes of `file`, as it was opened, to a new
/// `destination`, and says how many bytes that was. A file that shrank since
/// gives fewer: a positive count is a prefix of what was written, and none
/// leaves no destination behind.
fn copy_opened(file: &File, len: u64, destination: &Path) -> Result<Option<u64>> {
    let mut copy = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    let copied = io::copy(&mut file.take(len), &mut copy)
        .with_context(|| format!("copying to {}", destination.display()))?;
    if copied == 0 {
        drop(copy);
        fs::remove_file(destination)
            .with_context(|| format!("removing the empty {}", destination.display()))?;
        return Ok(None);
    }
    Ok(Some(copied))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};

    use super::{ChromiumReport, consts, copy_opened};

    /// A report nobody wants removes nothing from its directories: a file
    /// another writer put in them, under a name its steps also write, stays
    /// as it was.
    #[test]
    fn an_unwanted_report_removes_nothing_from_its_directories() {
        let build = tempfile::tempdir().expect("create a build");
        let report = ChromiumReport::prepare(build.path(), false, &|_| None)
            .expect("prepare a report outside a job");
        let capture = report.capture.clone();
        let driver = capture.join(consts::CHROMIUM_DRIVER_LOG);
        let foreign = capture
            .join(consts::CHROMIUM_NETLOG_DIR)
            .join("foreign.jsonl");
        fs::write(&driver, "own").expect("write the driver log");
        fs::write(&foreign, "foreign").expect("write a foreign network log");

        report.discard();

        assert_eq!(fs::read_to_string(&driver).unwrap(), "own");
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "foreign");
    }

    /// A file that held bytes when it was opened and none by the time it is
    /// copied leaves no copy: an empty file is never part of a report.
    #[test]
    fn a_file_emptied_after_it_was_opened_leaves_no_copy() {
        let dir = tempfile::tempdir().expect("create a directory");
        let source = dir.path().join("web_observer.jsonl");
        fs::write(&source, "net").expect("write a network log");
        let file = File::open(&source).expect("open the network log");
        let len = file.metadata().expect("read its length").len();
        File::create(&source).expect("empty the network log");
        let destination = dir.path().join("copy.jsonl");

        assert_eq!(
            copy_opened(&file, len, &destination).expect("copy what is left"),
            None
        );
        assert!(!destination.exists());
    }

    /// A file that shrank after it was opened gives what is left of it, a
    /// prefix of what was written.
    #[test]
    fn a_file_shrunk_after_it_was_opened_gives_its_prefix() {
        let dir = tempfile::tempdir().expect("create a directory");
        let source = dir.path().join("web_observer.jsonl");
        fs::write(&source, "network").expect("write a network log");
        let file = File::open(&source).expect("open the network log");
        let len = file.metadata().expect("read its length").len();
        fs::write(&source, "net").expect("shrink the network log");
        let destination = dir.path().join("copy.jsonl");

        assert_eq!(
            copy_opened(&file, len, &destination).expect("copy what is left"),
            Some(3)
        );
        assert_eq!(fs::read_to_string(&destination).unwrap(), "net");
    }
}
