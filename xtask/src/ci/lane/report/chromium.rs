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
    unistd::{self, UnlinkatFlags},
};
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
    capture: PathBuf,
    directories: Directories,
    destination: Destination,
}

/// The capture and network log directories, held open from their creation.
/// What the report copies, and what emptying it unlinks, is reached through
/// these, so a name pointed elsewhere during the run cannot make another
/// directory this run's report or the one emptying it reaches.
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
        let parent = open_directory(fcntl::AT_FDCWD, &build)
            .with_context(|| format!("opening {}", build.display()))?;
        // Made and opened relative to the build's descriptor, and nothing
        // removes it by name: a run that ends without its report leaves it.
        let (held, capture) = Builder::new()
            .prefix(consts::REPORT_PREFIX)
            .make_in(&build, |path| {
                let name = Path::new(path.file_name().unwrap_or(path.as_os_str()));
                stat::mkdirat(&parent, name, Mode::S_IRWXU)?;
                Ok(open_directory(&parent, name)?)
            })
            .and_then(|made| made.keep().map_err(|error| error.error))
            .with_context(|| format!("creating a capture directory in {}", build.display()))?;
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
                report.directories.empty(&report.capture);
            }
            Destination::InPlace => {
                info!(
                    "this run's Chromium report is in {}",
                    report.capture.display()
                );
            }
        }
    }

    /// Empties a report nobody wants.
    pub(super) fn discard(self) {
        if let Some(report) = self.still_named() {
            report.directories.empty(&report.capture);
        }
    }

    /// This report, while its capture directory still names the directories
    /// the steps were given. Otherwise it is left as it stands: whatever took
    /// their place is not this run's to upload or to remove.
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
            "{} {problem}; nothing in it is uploaded or removed",
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

    /// Unlinks every file in the network log directory and in the capture,
    /// through the descriptors held since they were made. A name is unlinked
    /// only inside these directories and never as a directory, so whatever
    /// took either directory's name, and any directory put in them, stays;
    /// the emptied directories stay too, for the build's own lifecycle.
    fn empty(&self, capture: &Path) {
        for dir in [&self.netlog, &self.capture] {
            let names = match names_in(dir) {
                Ok(names) => names,
                Err(error) => {
                    warn!(
                        "this run's Chromium capture in {} stays: {error:#}",
                        capture.display()
                    );
                    continue;
                }
            };
            for name in names {
                match unistd::unlinkat(dir, name.as_os_str(), UnlinkatFlags::NoRemoveDir) {
                    Ok(()) | Err(Errno::ENOENT | Errno::EISDIR | Errno::EPERM) => {}
                    Err(error) => warn!(
                        "{} stays in this run's Chromium capture in {}: {error}",
                        Path::new(&name).display(),
                        capture.display()
                    ),
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ChromiumReport, consts};

    /// Emptying a report reaches only the directories it made, whatever
    /// their names point at by then: here both were moved aside and foreign
    /// ones took their names after the report was admitted. The foreign bytes
    /// stay, and the moved capture loses its files but stays as a directory.
    #[test]
    fn emptying_a_report_leaves_whatever_took_its_names() {
        let build = tempfile::tempdir().expect("create a build");
        let report = ChromiumReport::prepare(build.path(), false, &|_| None)
            .expect("prepare a report outside a job");
        let capture = report.capture.clone();
        let netlog = capture.join(consts::CHROMIUM_NETLOG_DIR);
        fs::write(capture.join(consts::CHROMIUM_DRIVER_LOG), "own").expect("write the driver log");
        fs::write(netlog.join("own.jsonl"), "own").expect("write a network log");

        let aside = build.path().join("aside");
        fs::rename(&capture, &aside).expect("move the capture aside");
        fs::create_dir_all(&netlog).expect("put a foreign capture in its place");
        fs::write(capture.join(consts::CHROMIUM_DRIVER_LOG), "foreign")
            .expect("write a foreign driver log");
        fs::write(netlog.join("own.jsonl"), "foreign").expect("write a foreign network log");

        report.directories.empty(&report.capture);

        for foreign in [
            capture.join(consts::CHROMIUM_DRIVER_LOG),
            netlog.join("own.jsonl"),
        ] {
            assert_eq!(
                fs::read_to_string(&foreign).unwrap(),
                "foreign",
                "{}",
                foreign.display()
            );
        }
        assert!(!aside.join(consts::CHROMIUM_DRIVER_LOG).exists());
        assert_eq!(
            fs::read_dir(aside.join(consts::CHROMIUM_NETLOG_DIR))
                .unwrap()
                .count(),
            0
        );
    }
}
