use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::BuildDir;
use crate::{ci::environment::ci_in, consts};

/// A lane asking for a build directory of its own, and how long that
/// directory keeps a build unit the lane stopped using.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LaneTarget<'a> {
    pub(crate) name: &'a str,
    pub(crate) window: Duration,
}

/// Where a lane builds. These are two environments, not two attempts: each
/// has exactly one answer.
#[derive(Debug)]
pub(crate) enum Target {
    /// A CI job: the executor names its build root, and the lane builds in a
    /// slot of its own behind the root's alias, held while this lives. The
    /// slots are in the root unless the executor says where its runners share
    /// them.
    Alias {
        alias: PathBuf,
        build: Box<BuildDir>,
    },
    /// Anywhere else: wherever Cargo was told to build, or the checkout.
    Named(Option<PathBuf>),
}

impl Target {
    /// Enters `lane`'s build directory when `var` reads a CI job's
    /// environment, and links the checkout's `target` to it.
    ///
    /// # Errors
    ///
    /// When the build directory cannot be entered or the checkout's `target`
    /// cannot be linked to it.
    pub(crate) fn enter(
        checkout: &Path,
        lane: LaneTarget<'_>,
        var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        match var("CARGO_TARGET_DIR") {
            Some(root) if ci_in(var) => {
                let root = PathBuf::from(root);
                let alias = root.join(consts::BUILD_ALIAS);
                let slots = var(consts::BUILD_SLOTS_ENV).map_or(root, PathBuf::from);
                let build = BuildDir::enter(checkout, &alias, &slots, lane.name, lane.window)?;
                // Artifact paths name the checkout's `target`; Cargo is told
                // the alias, the one path every lane's compilations share.
                if let Some(slot) = exposed_slot(&build) {
                    expose_build_target(checkout, slot)?;
                }
                Ok(Self::Alias {
                    alias,
                    build: Box::new(build),
                })
            }
            named => Ok(Self::Named(named.map(PathBuf::from))),
        }
    }

    /// The directory Cargo is told to build in, when this process names one.
    pub(crate) fn cargo_dir(&self) -> Option<&Path> {
        match self {
            Self::Alias { alias, .. } => Some(alias),
            Self::Named(dir) => dir.as_deref(),
        }
    }

    pub(crate) fn report_root(&self, checkout: &Path, ci: bool) -> Result<PathBuf> {
        match self {
            Self::Alias { build, .. } => {
                Ok(exposed_slot(build).map_or_else(|| checkout.join("target"), Path::to_path_buf))
            }
            Self::Named(_) if ci => bail!("a CI report producer needs an acquired target"),
            Self::Named(_) => Ok(checkout.join("target")),
        }
    }
}

fn exposed_slot(build: &BuildDir) -> Option<&Path> {
    (!cfg!(windows)).then(|| build.path().as_path())
}

/// Links the checkout's `target` to the build directory, so reports and
/// artifacts collected from the checkout find what Cargo built there. The
/// checkout is the job's own, so whatever stands at `target` is replaced.
/// Windows keeps its build target in the checkout.
fn expose_build_target(checkout: &Path, build: &Path) -> Result<()> {
    let target = checkout.join("target");
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(&target)
                .with_context(|| format!("replacing stale CI target link {}", target.display()))?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&target)
                .with_context(|| format!("removing legacy checkout target {}", target.display()))?;
        }
        Ok(_) => bail!("CI target path is not a directory: {}", target.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading CI target link {}", target.display()));
        }
    }
    create_target_link(build, &target)
}

#[cfg(unix)]
fn create_target_link(backing: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(backing, target).with_context(|| {
        format!(
            "linking stable CI target {} to {}",
            target.display(),
            backing.display()
        )
    })
}

#[cfg(not(unix))]
fn create_target_link(_backing: &Path, _target: &Path) -> Result<()> {
    unreachable!("Windows keeps its build target in the checkout")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::build_dir::fixture::git_checkout;

    fn environment<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    fn lane() -> LaneTarget<'static> {
        LaneTarget {
            name: "lint",
            window: consts::DAY,
        }
    }

    /// A CI job is told its build root, the path both layouts a host serves
    /// are told, and builds behind the alias inside it. Anywhere else the
    /// directory Cargo was told is where it builds.
    #[cfg(unix)]
    #[test]
    fn only_a_ci_job_builds_behind_the_alias_in_the_root_cargo_was_told() {
        let checkout = git_checkout(&[]);
        let builds = tempfile::tempdir().unwrap();
        let root = builds.path().to_str().unwrap();

        match Target::enter(
            checkout.path(),
            lane(),
            &environment(&[("CARGO_TARGET_DIR", root)]),
        )
        .unwrap()
        {
            Target::Named(Some(dir)) => assert_eq!(dir, builds.path()),
            other => panic!("outside a CI job a lane builds where Cargo was told, not {other:?}"),
        }
        assert!(
            fs::read_dir(builds.path()).unwrap().next().is_none(),
            "outside a CI job nothing is made in the root Cargo was told"
        );

        match Target::enter(
            checkout.path(),
            lane(),
            &environment(&[("CI", "true"), ("CARGO_TARGET_DIR", root)]),
        )
        .unwrap()
        {
            Target::Alias { alias, build } => {
                assert_eq!(alias, builds.path().join(consts::BUILD_ALIAS));
                assert_eq!(build.path().parent(), Some(builds.path()));
            }
            other @ Target::Named(_) => {
                panic!("a CI job builds behind its executor's alias, not {other:?}")
            }
        }
        assert!(matches!(
            Target::enter(checkout.path(), lane(), &environment(&[("CI", "true")])).unwrap(),
            Target::Named(None)
        ));
    }

    /// Runners that share their lanes' builds are told where those are; the
    /// alias Cargo is told stays in the job's own root.
    #[cfg(unix)]
    #[test]
    fn a_job_told_where_lanes_build_takes_a_slot_there_behind_its_own_alias() {
        let checkout = git_checkout(&[]);
        let builds = tempfile::tempdir().unwrap();
        let slots = tempfile::tempdir().unwrap();

        let target = Target::enter(
            checkout.path(),
            lane(),
            &environment(&[
                ("CI", "true"),
                ("CARGO_TARGET_DIR", builds.path().to_str().unwrap()),
                (consts::BUILD_SLOTS_ENV, slots.path().to_str().unwrap()),
            ]),
        )
        .unwrap();

        let Target::Alias { alias, build } = target else {
            panic!("a CI job builds behind its executor's alias, not {target:?}");
        };
        assert_eq!(alias, builds.path().join(consts::BUILD_ALIAS));
        assert_eq!(build.path().parent(), Some(slots.path()));
        assert_eq!(fs::read_link(&alias).unwrap(), *build.path());
    }

    #[cfg(unix)]
    #[test]
    fn jobs_build_at_their_physical_backing_and_expose_reports_in_the_checkout() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let first = root.path().join("cache/job-4711/cargo");
        let second = root.path().join("cache/job-4712/cargo");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("Cargo.toml"), "[workspace]").unwrap();
        fs::create_dir_all(project.join("target/xtask-self-cache")).unwrap();
        fs::write(project.join("target/stale"), "legacy checkout target").unwrap();
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        expose_build_target(&project, &first).unwrap();
        fs::write(project.join("target/first"), "owned by the first job").unwrap();
        expose_build_target(&project, &second).unwrap();
        fs::write(project.join("target/second"), "owned by the second job").unwrap();

        assert_eq!(
            fs::canonicalize(project.join("target")).unwrap(),
            fs::canonicalize(&second).unwrap()
        );
        assert!(!first.join("stale").exists());
        assert!(first.join("first").is_file());
        assert!(!first.join("second").exists());
        assert!(second.join("second").is_file());
        assert!(!second.join("first").exists());
    }

    #[cfg(unix)]
    #[test]
    fn one_build_keeps_its_native_output_path_across_checkouts() {
        let root = tempfile::tempdir().unwrap();
        let backing = root.path().join("lane/cargo");
        fs::create_dir_all(&backing).unwrap();
        for name in ["checkout-0", "checkout-1"] {
            let checkout = root.path().join(name);
            fs::create_dir_all(&checkout).unwrap();
            expose_build_target(&checkout, &backing).unwrap();
            assert_eq!(
                fs::canonicalize(checkout.join("target")).unwrap(),
                fs::canonicalize(&backing).unwrap()
            );
        }
    }
}
