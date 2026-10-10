use std::{fs, io, path::Path};

use anyhow::{Context, Result};

use super::garbage::targets;
use crate::consts;

/// A slot is held by one job at a time, so every report in it at entry is an
/// earlier job's. Removing them makes any report found later this job's own.
///
/// # Errors
///
/// When a target cannot be listed or its timing directory cannot be removed.
pub(super) fn clear(dir: &Path) -> Result<()> {
    for target in targets(dir)? {
        let reports = target.join(consts::CARGO_TIMINGS_DIR);
        match fs::remove_dir_all(&reports) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("removing {}", reports.display()));
            }
        }
    }
    Ok(())
}

/// Copies reports while the job still holds the slot, so the copy outlives
/// its release and the next job's clearing. It holds exactly the reports
/// [`clear`] would have removed, with their paths relative to the slot.
///
/// # Errors
///
/// When the destination cannot be removed, a target cannot be listed, or a
/// report cannot be read or copied.
pub(super) fn copy(dir: &Path, to: &Path) -> Result<bool> {
    match fs::remove_dir_all(to) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("removing {}", to.display())),
    }
    let mut copied = false;
    for target in targets(dir)? {
        let report = target
            .join(consts::CARGO_TIMINGS_DIR)
            .join(consts::CARGO_TIMING_FILE);
        match fs::metadata(&report) {
            Ok(metadata) if metadata.is_file() => {
                let relative = target.strip_prefix(dir).with_context(|| {
                    format!("making {} relative to {}", target.display(), dir.display())
                })?;
                let destination = to.join(relative).join(consts::CARGO_TIMINGS_DIR);
                fs::create_dir_all(&destination)
                    .with_context(|| format!("creating {}", destination.display()))?;
                let destination = destination.join(consts::CARGO_TIMING_FILE);
                fs::copy(&report, &destination).with_context(|| {
                    format!("copying {} to {}", report.display(), destination.display())
                })?;
                copied = true;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", report.display()));
            }
        }
    }
    Ok(copied)
}
