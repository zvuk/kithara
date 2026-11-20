use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
};

use anyhow::{Context, Result, bail};

#[cfg(unix)]
use super::chromium::ChromiumReport;
use crate::{
    ci::build_dir::Target,
    config::{CiLaneArtifact, CiLaneConfig, LaneReport},
};

/// A declared lane's report, captured for the run that writes it.
///
/// `ci lane` and `ci run` both take a declared lane through here, so either
/// way in its steps are told where to write, and a wanted report is handed
/// over while the build it was written in is still held.
pub(crate) struct Capture {
    report: ChromiumReport,
    artifact: CiLaneArtifact,
}

impl Capture {
    /// Prepares the report `lane` declares, in the build `target` holds.
    ///
    /// A GitHub job hands its report to later steps, so it captures one only
    /// in a build it holds: a target named by path is shared with whatever
    /// else names that path. Anywhere else the report stays where it was
    /// written, and a named target is where that is.
    ///
    /// # Errors
    ///
    /// When the lane declares a report this run cannot capture.
    pub(crate) fn for_lane(
        lane: &CiLaneConfig,
        target: &Target,
        root: &Path,
        var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Option<Self>> {
        let Some((artifact, report)) = lane
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.report.map(|report| (artifact, report)))
        else {
            return Ok(None);
        };
        let text = |name: &str| var(name).and_then(|value| value.into_string().ok());
        let github = crate::job::github_in(&text);
        let build = match target {
            Target::Alias { build, .. } => build.path().to_path_buf(),
            Target::Named(_) if github => bail!(
                "a GitHub job captures its {} report in the build it holds, and it holds none: CARGO_TARGET_DIR names no build root",
                report.name()
            ),
            Target::Named(dir) => dir
                .as_ref()
                .map_or_else(|| root.join("target"), |dir| root.join(dir)),
        };
        let report = match report {
            LaneReport::Chromium => ChromiumReport::prepare(&build, github, var)
                .context("preparing this run's Chromium report")?,
        };
        Ok(Some(Self {
            report,
            artifact: artifact.clone(),
        }))
    }

    /// What the lane's steps are told about where to write the report.
    pub(crate) fn env(&self) -> [(OsString, OsString); 2] {
        self.report.env()
    }

    /// Hands the report over when a lane that ended this way wants it, and
    /// removes it otherwise. Failing to hand it over never fails the lane:
    /// the job's own check reports a missing copy.
    pub(crate) fn finish(self, failed: bool) {
        if self.artifact.wanted(failed) {
            self.report.hand_over();
        } else {
            self.report.discard();
        }
    }
}

/// Tells later steps, through `GITHUB_ENV`, that `name` is this job's copy in
/// `dir`. A job cancelled before telling tells nothing.
///
/// The line goes out in one call, yet a short write can still leave part of
/// the path behind; a reader that needs the whole path checks how it ends.
pub(in crate::ci::lane) fn announce(env_file: &Path, name: &str, dir: &Path) -> io::Result<()> {
    let dir = dir
        .to_str()
        .filter(|dir| !dir.contains(['\r', '\n']))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the copy's path is not one line",
            )
        })?;
    let mut file = OpenOptions::new().append(true).open(env_file)?;
    file.write_all(format!("{name}={dir}\n").as_bytes())
}

/// Capturing a report reads its directories through descriptors held from
/// their creation, which this platform does not offer.
#[cfg(not(unix))]
enum ChromiumReport {}

#[cfg(not(unix))]
impl ChromiumReport {
    fn prepare(
        _build: &Path,
        _github: bool,
        _var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        bail!("a Chromium report is captured on Unix only")
    }

    fn env(&self) -> [(OsString, OsString); 2] {
        match *self {}
    }

    fn hand_over(self) {
        match self {}
    }

    fn discard(self) {
        match self {}
    }
}
