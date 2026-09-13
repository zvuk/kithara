use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::Args;
use kithara_devtools::Ctx;
use tracing::info;

/// What this programme is pinned to: the price lists it judges against, the
/// control it photographs on its own, and the sets it owns.
struct Consts;

impl Consts {
    /// The one control this programme photographs by itself, so the shortest
    /// capture path runs somewhere. Any page that draws a control by a known
    /// path would do; this one is pinned so a page that stops drawing it says
    /// so.
    const ELEMENT_PAGE: &'static str = "clock";
    const ELEMENT_PATH: &'static str = "clock-components/title";
    /// What a page of the gallery, and a shipped studio page, are allowed to
    /// differ by before the programme ends non-zero.
    const GALLERY_BUDGET: &'static str = "crates/kithara-ui/examples/gallery/parity-budget.txt";
    /// The sets this programme writes, cleared before it starts so a set left
    /// by an earlier run cannot be compared as if this run had taken it.
    const SETS: [&'static str; 5] = ["iced", "masonry", "masks", "parts", "studio"];
    const STUDIO_BUDGET: &'static str = "crates/kithara-app/assets/ui/parity-budget.txt";
    /// Where the studio capture is told to write its two sets. It is driven
    /// from a test, and a test has no command line of its own to be told on.
    const STUDIO_CAPTURE: &'static str = "KITHARA_STUDIO_CAPTURE";
}

#[derive(Debug, Args)]
pub(crate) struct ParityArgs {
    /// Where the sets, their masks and their pictures land.
    #[arg(long, default_value = "target/parity")]
    dir: PathBuf,
}

/// Photographs the same documents through both hosts and compares the sets.
///
/// # Errors
/// Fails when a set cannot be photographed, when a page differs by more than
/// its budget allows, or when a page is missing from one of the two sets.
pub(crate) fn run(args: &ParityArgs, ctx: &Ctx) -> Result<()> {
    let dir = rooted(&ctx.root, &args.dir);
    for set in Consts::SETS {
        let path = dir.join(set);
        if path.exists() {
            fs::remove_dir_all(&path).with_context(|| format!("clearing {}", path.display()))?;
        }
    }

    let gallery = Gallery { root: &ctx.root };
    gallery.shoot("the gallery through iced", &dir.join("iced"), &[])?;
    gallery.shoot(
        "the gallery through masonry",
        &dir.join("masonry"),
        &["--host", "retained"],
    )?;
    gallery.compare(
        "the gallery, host against host",
        [&dir.join("iced"), &dir.join("masonry"), &dir.join("masks")],
        &ctx.root.join(Consts::GALLERY_BUDGET),
    )?;
    gallery.element(&dir.join("parts"))?;

    let studio = dir.join("studio");
    studio_capture(&ctx.root, &studio)?;
    gallery.compare(
        "the shipped studio pages, host against host",
        [
            &studio.join("iced"),
            &studio.join("masonry"),
            &studio.join("masks"),
        ],
        &ctx.root.join(Consts::STUDIO_BUDGET),
    )
}

fn rooted(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// The gallery example built with both hosts in it, which is what photographs
/// a set through either one and what compares two sets.
struct Gallery<'root> {
    root: &'root Path,
}

impl Gallery<'_> {
    fn command(&self) -> Command {
        let mut command = Command::new("cargo");
        command.current_dir(self.root).args([
            "run",
            "-p",
            "kithara-ui",
            "--example",
            "gallery",
            "--features",
            "kithara-ui/capture,kithara-ui/masonry",
            "--",
        ]);
        command
    }

    fn shoot(&self, what: &str, dir: &Path, host: &[&str]) -> Result<()> {
        let mut command = self.command();
        command.args(host).arg("--shoot").arg(dir);
        finish(command, what)
    }

    fn compare(&self, what: &str, sets: [&Path; 3], budget: &Path) -> Result<()> {
        let mut command = self.command();
        command
            .arg("--compare")
            .args(sets)
            .arg("--budget")
            .arg(budget);
        finish(command, what)
    }

    fn element(&self, dir: &Path) -> Result<()> {
        let mut command = self.command();
        command
            .args(["--host", "retained"])
            .arg("--shoot")
            .arg(dir)
            .args([
                "--page",
                Consts::ELEMENT_PAGE,
                "--element",
                Consts::ELEMENT_PATH,
            ]);
        finish(command, "one control of one gallery page")
    }
}

/// The two sets of the pages the app ships, taken by the app's own test: the
/// documents are the app's, and only the app knows what to read behind them.
fn studio_capture(root: &Path, dir: &Path) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .env(Consts::STUDIO_CAPTURE, dir)
        .args([
            "test",
            "-p",
            "kithara-app",
            "--lib",
            "--no-default-features",
            "--features",
            "backend-cpal,masonry,stretch-signalsmith",
            "studio_capture_writes_both_hosts",
            "--",
            "--nocapture",
        ]);
    finish(command, "the shipped studio pages through both hosts")
}

/// Runs one step and says what it was when it fails, because the command line
/// alone reads as cargo's rather than as this programme's.
fn finish(mut command: Command, what: &str) -> Result<()> {
    info!(step = what, "parity");
    let status = command
        .status()
        .with_context(|| format!("running {what}"))?;
    if !status.success() {
        bail!("{what} failed with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::Path};

    use super::{Gallery, rooted};

    #[test]
    fn a_relative_folder_is_taken_from_the_workspace_root() {
        assert_eq!(
            rooted(Path::new("/work"), Path::new("target/parity")),
            Path::new("/work/target/parity")
        );
    }

    #[test]
    fn an_absolute_folder_is_taken_as_it_stands() {
        assert_eq!(
            rooted(Path::new("/work"), Path::new("/elsewhere")),
            Path::new("/elsewhere")
        );
    }

    /// One binary takes both sets and compares them, so a difference between
    /// them is a difference between the hosts rather than between two builds.
    #[test]
    fn every_step_runs_the_gallery_built_with_both_hosts() {
        let command = Gallery {
            root: Path::new("/work"),
        }
        .command();
        let args: Vec<_> = command.get_args().map(OsStr::to_string_lossy).collect();
        assert!(
            args.iter()
                .any(|arg| arg == "kithara-ui/capture,kithara-ui/masonry")
        );
    }
}
