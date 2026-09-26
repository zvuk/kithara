use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::{Parser, ValueEnum};
use iced::Size;
use kithara_platform::time::Duration;
use kithara_ui::capture::{Film, Geometry, read_geometry};
use num_traits::cast::AsPrimitive;

use crate::{capture::Shot, fixture::consts};

/// Which host draws the gallery.
///
/// The retained host is a build the `masonry` feature turns on; without it
/// there is only one host to name. Both read the same documents, so the choice
/// is the shell and nothing else.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum Host {
    /// iced: the tree is rebuilt from the readings on every message.
    #[default]
    Immediate,
    /// masonry and Vello: the tree is kept and told what changed.
    #[cfg(feature = "masonry")]
    Retained,
}

/// The smallest size a window can be dragged to unless a run asks for another.
pub(crate) const MIN_WINDOW: Extent = Extent {
    height: consts::MIN_HEIGHT,
    width: consts::MIN_WIDTH,
};

/// The size the gallery opens at unless a run asks for another.
pub(crate) const WINDOW: Extent = Extent {
    height: consts::HEIGHT,
    width: consts::WIDTH,
};

/// A size written the way a person writes one: `1300x720`.
#[derive(Clone, Copy, Debug, PartialEq, derive_more::Display)]
#[display("{}x{}", width, height)]
pub(crate) struct Extent {
    pub(crate) height: f32,
    pub(crate) width: f32,
}

impl From<Extent> for Size {
    fn from(extent: Extent) -> Self {
        Self::new(extent.width, extent.height)
    }
}

impl FromStr for Extent {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (width, height) = text
            .split_once('x')
            .ok_or_else(|| format!("expected <width>x<height>, got {text}"))?;
        Ok(Self {
            height: side(height)?,
            width: side(width)?,
        })
    }
}

/// One side of a size, which is a length and so cannot be negative or absent.
fn side(text: &str) -> Result<f32, String> {
    let side = text
        .parse::<f32>()
        .map_err(|_| format!("a side must be a number, got {text}"))?;
    if side > 0.0 {
        Ok(side)
    } else {
        Err(format!("a side must be greater than zero, got {text}"))
    }
}

/// The gallery: the toolkit's pages, shown or photographed.
#[derive(Clone, Debug, Parser)]
#[command(name = "gallery", about = "The Kithara UI gallery")]
pub(crate) struct Args {
    /// The smallest size the window can be dragged to, so a page can be shown
    /// the room its adaptive and revealed cells answer.
    #[arg(long, value_name = "WxH", default_value_t = MIN_WINDOW)]
    pub(crate) min_size: Extent,

    /// The size the gallery opens and photographs at.
    #[arg(long, value_name = "WxH", default_value_t = WINDOW)]
    pub(crate) size: Extent,

    /// Which host draws the gallery.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) host: Host,

    /// Fail the comparison where a page differs by more than this file allows.
    /// Without one the comparison only reports.
    #[arg(long, value_name = "FILE", requires = "compare")]
    pub(crate) budget: Option<PathBuf>,

    /// Compare two folders of photographs, writing the differences to a third.
    #[arg(long, num_args = 3, value_names = ["A", "B", "OUT"])]
    pub(crate) compare: Option<Vec<PathBuf>>,

    /// Photograph the one control at this document path instead of whole
    /// pages. Takes one `--page`, and the host that keeps what it draws.
    #[arg(
        long,
        value_name = "PATH",
        requires = "shoot",
        conflicts_with = "windowed"
    )]
    pub(crate) element: Option<String>,

    /// Photograph the pages into this folder instead of showing them.
    #[arg(long, value_name = "DIR")]
    pub(crate) shoot: Option<PathBuf>,

    /// A page to photograph, repeated. Every page when none is named.
    #[arg(long = "page", value_name = "NAME", requires = "shoot")]
    pub(crate) pages: Vec<Shot>,

    /// Photograph through a window on a display rather than off-screen. The
    /// immediate host only: the retained one photographs off-screen either way.
    #[arg(long, requires = "shoot")]
    pub(crate) windowed: bool,

    /// The scale a photograph is taken at. One means a page's pixels are its
    /// points, so a difference between two sets is a difference in what was
    /// drawn rather than in how it was sampled.
    #[arg(long, value_name = "N", default_value_t = consts::SCALE)]
    pub(crate) scale: f32,

    /// How long one frame of a moving page takes.
    #[arg(long, value_name = "MS", default_value_t = consts::STRESS_TICK_MS)]
    pub(crate) tick: u64,

    /// How many photographs to take of each page. More than one shows a page
    /// that moves at more than the moment it opens at.
    #[arg(long, value_name = "N", default_value_t = 1, requires = "shoot")]
    pub(crate) photos: usize,

    /// How many frames run between two photographs of a page.
    #[arg(long, value_name = "N", default_value_t = 0, requires = "shoot")]
    pub(crate) steps: usize,
}

impl Args {
    /// The film this run asked for: the pages it named, photographed at the
    /// cadence it named. Every page once when it named neither.
    pub(crate) fn film(&self) -> Result<Film<Shot>, String> {
        let pages = if self.pages.is_empty() {
            Shot::all()
        } else {
            self.pages.clone()
        };
        Film::new(pages, self.photos, self.steps)
    }

    /// The frame a set in this folder is photographed in: whatever a capture
    /// already sitting there used, so a set taken on a display — where the
    /// scale is the screen's and not ours — is answered on its own terms.
    /// Otherwise the size and scale this run asked for.
    pub(crate) fn frame(&self, dir: &Path) -> Geometry {
        read_geometry(dir).unwrap_or_else(|| self.geometry())
    }

    /// The frame this run asked for.
    pub(crate) fn geometry(&self) -> Geometry {
        Geometry {
            height: AsPrimitive::<u32>::as_(self.size.height * self.scale),
            scale: self.scale.into(),
            width: AsPrimitive::<u32>::as_(self.size.width * self.scale),
        }
    }

    /// How far a clock moves in one frame.
    pub(crate) fn step(&self) -> Duration {
        Duration::from_millis(self.tick)
    }
}

#[cfg(test)]
mod tests {
    use std::iter::once;

    use clap::{CommandFactory, Parser as _};
    use kithara_test_utils::kithara;

    use super::{Args, Duration, Extent, Host, Path, Shot};
    use crate::sections::Page;

    fn args(flags: &[&str]) -> Args {
        Args::try_parse_from(once("gallery").chain(flags.iter().copied()))
            .expect("the flags are ones the gallery takes")
    }

    fn refused(flags: &[&str]) -> bool {
        Args::try_parse_from(once("gallery").chain(flags.iter().copied())).is_err()
    }

    fn shot(tab: Page) -> Shot {
        Shot { tab, module: None }
    }

    /// Clap checks its own wiring here rather than at the first run that trips
    /// over it: a flag requiring one the gallery does not have is a mistake in
    /// this file, not in the command line a person typed.
    #[kithara::test]
    fn the_command_line_is_well_formed() {
        Args::command().debug_assert();
    }

    /// A gallery nobody pointed at a folder shows its pages instead of
    /// photographing them somewhere nobody asked for.
    #[kithara::test]
    fn a_run_with_no_flags_photographs_nothing() {
        assert_eq!(args(&[]).shoot, None);
    }

    #[kithara::test]
    fn a_run_with_no_flags_compares_nothing() {
        assert_eq!(args(&[]).compare, None);
    }

    #[kithara::test]
    fn a_run_with_no_flags_draws_through_the_immediate_host() {
        assert_eq!(args(&[]).host, Host::Immediate);
    }

    #[kithara::test]
    fn a_capture_that_names_no_page_photographs_every_page() {
        let film = args(&["--shoot", "out"]).film().expect("a still set");
        assert_eq!(film.pages, Shot::all());
    }

    #[kithara::test]
    fn a_capture_photographs_the_pages_it_names_in_the_order_it_names_them() {
        let film = args(&["--shoot", "out", "--page", "motion", "--page", "lottie"])
            .film()
            .expect("both pages are ones the gallery has");
        assert_eq!(film.pages, vec![shot("motion"), shot("lottie")]);
    }

    #[kithara::test]
    fn a_page_the_gallery_does_not_have_is_refused() {
        assert!(refused(&["--shoot", "out", "--page", "nowhere"]));
    }

    #[kithara::test]
    fn a_film_of_several_photographs_with_no_time_between_them_is_refused() {
        assert!(
            args(&["--shoot", "out", "--photos", "2"]).film().is_err(),
            "two photographs of one moment are one photograph twice",
        );
    }

    #[kithara::test]
    fn a_film_runs_as_many_frames_between_photographs_as_it_asks_for() {
        let film = args(&["--shoot", "out", "--photos", "2", "--steps", "3"])
            .film()
            .expect("a film in motion");
        assert_eq!(film.steps, 3);
    }

    #[kithara::test]
    fn a_photograph_is_taken_at_the_size_it_was_asked_for() {
        let frame = args(&["--shoot", "out", "--size", "800x600"]).geometry();
        assert_eq!((frame.width, frame.height), (800, 600));
    }

    #[kithara::test]
    fn a_photograph_at_a_scale_carries_that_many_pixels_per_point() {
        let frame = args(&["--shoot", "out", "--size", "800x600", "--scale", "2"]).geometry();
        assert_eq!((frame.width, frame.height), (1600, 1200));
    }

    #[kithara::test]
    fn a_frame_takes_the_time_it_was_asked_for() {
        assert_eq!(
            args(&["--tick", "40"]).step(),
            Duration::from_millis(40),
            "a page in motion moves by the step this run named",
        );
    }

    /// A folder holding no set has no geometry to answer on, so the run's own
    /// size and scale stand.
    #[kithara::test]
    fn a_folder_with_no_set_in_it_is_photographed_at_the_size_this_run_asked_for() {
        let args = args(&["--shoot", "nowhere", "--size", "800x600"]);
        assert_eq!(args.frame(Path::new("nowhere")), args.geometry());
    }

    #[kithara::test]
    fn a_size_written_as_one_number_is_refused() {
        assert_eq!(
            "1300".parse::<Extent>(),
            Err("expected <width>x<height>, got 1300".to_owned())
        );
    }

    #[kithara::test]
    fn a_size_with_a_side_of_nothing_is_refused() {
        assert!("0x600".parse::<Extent>().is_err());
    }

    #[kithara::test]
    fn a_run_with_no_flags_photographs_no_control() {
        assert_eq!(args(&[]).element, None);
    }

    /// A control is cut out of a photograph taken off-screen; a run that asks
    /// for a window is asking for the whole window.
    #[kithara::test]
    fn a_control_photographed_through_a_window_is_refused() {
        assert!(refused(&[
            "--shoot",
            "out",
            "--element",
            "deck/play",
            "--windowed"
        ]));
    }

    /// A page named without a folder to write it to photographs nothing, so
    /// the command line says so instead of running the gallery as if the flag
    /// had not been typed.
    #[kithara::test]
    fn a_capture_parameter_without_a_folder_is_refused() {
        assert!(refused(&["--page", "motion"]));
        assert!(refused(&["--element", "deck/play"]));
    }
}
