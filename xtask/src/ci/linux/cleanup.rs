use anyhow::{Result, bail};
use tracing::info;

use crate::ci::process::Process;

/// Build cache older than this is rebuilt faster than it is worth keeping.
const BUILD_CACHE_AGE: &str = "168h";

/// Reclaim what this project left behind, and nothing else.
///
/// The machine is shared: other stacks keep images and volumes here, so a
/// blanket `docker system prune` would take theirs. Volumes are left alone
/// entirely — `kithara-ci-target` and `kithara-ci-cargo-home` are the caches
/// this exists to protect, and the orphans beside them belong to a setup this
/// code does not own.
///
/// What to keep is named by the caller rather than read from the pins, because
/// this runs from a timer and the pins move with the repository. Read there,
/// a bumped pin nobody has installed yet would name the running fleet's image
/// as superseded and take it out from under the services.
pub(super) fn run(process: &Process, keep: &[String]) -> Result<()> {
    // Every project image would be superseded by an empty list, and this is
    // the one caller whose mistakes are unattended.
    if keep.is_empty() {
        bail!("cleanup was given no image to keep; reinstall the services to name them");
    }

    let listed = process.capture(
        "docker",
        &[
            "images",
            "kithara-ci*",
            "--format",
            "{{.Repository}}:{{.Tag}}",
        ],
        "list project images",
    )?;
    let superseded = superseded(&listed, keep);
    for image in &superseded {
        process.best_effort("docker", &["rmi", image], "remove a superseded image");
    }
    info!(
        removed = superseded.len(),
        kept = keep.len(),
        "superseded project images removed"
    );

    process.best_effort(
        "docker",
        &[
            "builder",
            "prune",
            "--force",
            "--filter",
            &format!("until={BUILD_CACHE_AGE}"),
        ],
        "prune the build cache",
    );
    Ok(())
}

/// Which of the project's images nothing on this machine is installed to run.
fn superseded<'a>(listed: &'a str, keep: &[String]) -> Vec<&'a str> {
    listed
        .lines()
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .filter(|image| !keep.iter().any(|kept| kept == image))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTED: &str = "kithara-ci:linux-20260729\n\
                          kithara-ci:linux-20260806d\n\
                          kithara-ci-android:linux-20260806c\n\
                          kithara-ci-android-runner:linux-20260806c\n\
                          kithara-ci-runner:linux-20260806d\n";

    fn keep(images: &[&str]) -> Vec<String> {
        images.iter().map(|image| (*image).to_owned()).collect()
    }

    #[test]
    fn a_generation_the_fleet_no_longer_runs_is_superseded() {
        let kept = keep(&[
            "kithara-ci:linux-20260806d",
            "kithara-ci-runner:linux-20260806d",
            "kithara-ci-android:linux-20260806c",
            "kithara-ci-android-runner:linux-20260806c",
        ]);
        assert_eq!(superseded(LISTED, &kept), ["kithara-ci:linux-20260729"]);
    }

    /// The emulator lane runs a generation of its own, and a rule that kept one
    /// tag would delete the image half the fleet is started from.
    #[test]
    fn a_lane_on_an_older_tag_than_the_others_keeps_its_image() {
        let kept = keep(&[
            "kithara-ci:linux-20260806d",
            "kithara-ci-runner:linux-20260806d",
            "kithara-ci-android:linux-20260806c",
            "kithara-ci-android-runner:linux-20260806c",
        ]);
        assert!(!superseded(LISTED, &kept).contains(&"kithara-ci-android:linux-20260806c"));
    }

    /// The pins move with the repository and the timer does not, so cleanup is
    /// told what is installed. Were it told what is merely pinned, the newer
    /// tag would name the running fleet's image as superseded.
    #[test]
    fn an_image_the_fleet_runs_survives_a_pin_it_has_not_been_given() {
        let bumped = keep(&[
            "kithara-ci:linux-20260810a",
            "kithara-ci-runner:linux-20260810a",
        ]);
        assert!(superseded(LISTED, &bumped).contains(&"kithara-ci-runner:linux-20260806d"));

        let installed = keep(&["kithara-ci-runner:linux-20260806d"]);
        assert!(!superseded(LISTED, &installed).contains(&"kithara-ci-runner:linux-20260806d"));
    }
}
