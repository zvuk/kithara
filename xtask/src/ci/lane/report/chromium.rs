use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::{
        fd::AsFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use nix::{dir::Dir, errno::Errno, fcntl, sys::stat::Mode};
use tempfile::{Builder, TempDir};
use tracing::{info, warn};

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

/// One run's Chromium report: captured in a fresh directory of the build the
/// run holds, so a job reusing that build later writes elsewhere.
pub(super) struct ChromiumReport {
    capture: TempDir,
    directories: Directories,
    destination: Destination,
}

/// The capture and network log directories, held open from their creation.
/// What the report copies is read through these, so a name pointed elsewhere
/// during the run cannot make another directory this run's report.
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
    /// Makes the capture directory in `build` and, in a GitHub job, the job's
    /// copy. A GitHub job without its storage fails here, before the browser
    /// runs, rather than running without a report.
    pub(super) fn prepare(
        build: &Path,
        github: bool,
        var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        fs::create_dir_all(build).with_context(|| format!("creating {}", build.display()))?;
        let build =
            fs::canonicalize(build).with_context(|| format!("resolving {}", build.display()))?;
        // Removed only by `discard`, once it is still the capture: a run that
        // unwinds before then leaves it in the build rather than removing
        // whatever its name points at by then.
        let capture = Builder::new()
            .prefix(consts::REPORT_PREFIX)
            .disable_cleanup(true)
            .tempdir_in(&build)
            .with_context(|| format!("creating a capture directory in {}", build.display()))?;
        ensure!(
            capture
                .path()
                .to_str()
                .is_some_and(|path| !path.contains(char::is_whitespace)),
            "chromedriver cannot take {} as one argument",
            capture.path().display()
        );
        let held = open_directory(fcntl::AT_FDCWD, capture.path())
            .with_context(|| format!("opening {}", capture.path().display()))?;
        fs::create_dir(capture.path().join(consts::CHROMIUM_NETLOG_DIR)).with_context(|| {
            format!(
                "creating the network log directory in {}",
                capture.path().display()
            )
        })?;
        let netlog =
            open_directory(&held, Path::new(consts::CHROMIUM_NETLOG_DIR)).with_context(|| {
                format!(
                    "opening the network log directory in {}",
                    capture.path().display()
                )
            })?;
        let destination = if github {
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
            Destination::Job {
                copy,
                env_file: PathBuf::from(env_file),
            }
        } else {
            Destination::InPlace
        };
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
        driver.push(self.capture.path().join(consts::CHROMIUM_DRIVER_LOG));
        [
            (OsString::from("CHROMEDRIVER_ARGS"), driver),
            (
                OsString::from(consts::CHROMIUM_NETLOG_ENV),
                self.capture
                    .path()
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
                remove(report.capture.path());
            }
            Destination::InPlace => {
                // Kept before logging: a disabled level skips its arguments.
                let kept = report.capture.keep();
                info!("this run's Chromium report is in {}", kept.display());
            }
        }
    }

    /// Removes a report nobody wants.
    pub(super) fn discard(self) {
        if let Some(report) = self.still_named() {
            remove(report.capture.path());
        }
    }

    /// This report, while its capture directory still names the directories
    /// the steps were given. Otherwise it is left as it stands: whatever took
    /// their place is not this run's to upload or to remove.
    fn still_named(self) -> Option<Self> {
        let problem = match self.directories.named_by(self.capture.path()) {
            Ok(true) => return Some(self),
            Ok(false) => "no longer names the directories this run's steps were given".to_owned(),
            Err(error) => {
                format!(
                    "cannot be checked against the directories this run's steps were given: {error}"
                )
            }
        };
        let kept = self.capture.keep();
        warn!(
            "{} {problem}; nothing in it is uploaded or removed",
            kept.display()
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

/// Removes a capture that `still_named` found to be this run's.
fn remove(capture: &Path) {
    if let Err(error) = fs::remove_dir_all(capture) {
        warn!(
            "this run's Chromium capture stays in {}: {error}",
            capture.display()
        );
    }
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
/// was opened, and says how many bytes that was. A file this run did not
/// write alone - missing, a link, not a regular file, shared with another
/// name - or an empty one is not copied.
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
    let mut copy = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    io::copy(&mut (&file).take(metadata.len()), &mut copy)
        .map(Some)
        .with_context(|| {
            format!(
                "copying {} to {}",
                Path::new(name).display(),
                destination.display()
            )
        })
}
