use std::path::PathBuf;

use anyhow::{Result, bail};
use tracing::info;

use super::{container::Container, profile::LinuxHost};
use crate::ci::{build_cache, process::Process};

/// Build cache older than this is rebuilt faster than it is worth keeping.
const BUILD_CACHE_AGE: &str = "168h";

/// Reclaim what this project left behind, and nothing else.
///
/// The machine is shared: other stacks keep images and volumes here, so a
/// blanket `docker system prune` would take theirs. A live cache stays and is
/// held to a budget instead: the per-runner target directories under the
/// configured cache root are trimmed here, `kithara-ci-sccache` and
/// `kithara-ci-fixtures` are what this exists to protect, and Cargo home is
/// never touched. A volume of this
/// project's that no container is attached to any more is not a cache but a
/// leftover, and it is reclaimed.
///
/// What to keep is named by the caller rather than read from the pins, because
/// this runs from a timer and the pins move with the repository. Read there,
/// a bumped pin nobody has installed yet would name the running fleet's image
/// as superseded and take it out from under the services.
pub(super) fn run(process: &Process, host: &LinuxHost, keep: &[String]) -> Result<()> {
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

    let dangling = process.capture(
        "docker",
        &[
            "volume",
            "ls",
            "--filter",
            "name=kithara-ci",
            "--filter",
            "dangling=true",
            "--format",
            "{{.Name}}",
        ],
        "list this project's unattached volumes",
    )?;
    let orphans = orphaned_volumes(&dangling);
    for volume in &orphans {
        process.best_effort("docker", &["volume", "rm", volume], "remove a dead volume");
    }
    info!(removed = orphans.len(), "dead project volumes removed");

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
    let target_dirs = target_dirs(host);
    build_cache::enforce_budget(&target_dirs, host.build_cache_budget_bytes()?)?;
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

/// This project's volumes that no container is attached to any more.
///
/// The live per-runner caches (`kithara-ci-target-<owner>-<n>`,
/// `kithara-ci-sccache`, `kithara-ci-fixtures`) all carry a link and are never
/// listed here — Docker's own `dangling` filter is the authority on that, not a
/// name pattern. What this catches is a generation the fleet has moved off:
/// the single shared `kithara-ci-target` left behind when the runners went to
/// one volume each held 231 GB on a 1.8 TB disk that was 99% full, and nothing
/// reclaimed it, because "leave the volumes alone" treated this project's own
/// corpse as if it belonged to a stack this code does not own. It does not: the
/// name says whose it is and the link count says it is dead.
fn orphaned_volumes(listed: &str) -> Vec<&str> {
    listed
        .lines()
        .map(str::trim)
        .filter(|volume| !volume.is_empty())
        .collect()
}

/// Where the live per-runner target caches sit on disk, so their contents can
/// be held to a budget.
fn target_dirs(host: &LinuxHost) -> Vec<PathBuf> {
    host.runners
        .iter()
        .map(|runner| Container::target_dir(host, runner))
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
    fn a_volume_no_container_holds_is_reclaimed() {
        // What `docker volume ls --filter dangling=true` answers: the live
        // per-runner caches carry a link and never appear, so this list is
        // already only the dead. Blank lines are what the empty case looks
        // like on the wire.
        let dangling = "kithara-ci-target\n\nkithara-ci-target-gerasim13-99\n";
        assert_eq!(
            orphaned_volumes(dangling),
            ["kithara-ci-target", "kithara-ci-target-gerasim13-99"],
            "every unattached project volume is named for removal"
        );
    }

    #[test]
    fn nothing_is_removed_when_every_volume_is_attached() {
        assert!(
            orphaned_volumes("").is_empty(),
            "an empty dangling list must not name a single volume"
        );
        assert!(
            orphaned_volumes("   \n\n").is_empty(),
            "whitespace is not a volume name"
        );
    }

    #[test]
    fn build_cache_budget_uses_the_profile_storage_root() {
        let host = crate::ci::linux::profile::tests::host_fixture();
        assert_eq!(
            target_dirs(&host),
            host.runners
                .iter()
                .map(|runner| host.cache_root.join("target").join(&runner.name))
                .collect::<Vec<_>>()
        );
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
