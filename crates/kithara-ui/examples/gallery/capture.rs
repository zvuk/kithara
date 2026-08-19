use std::{
    env,
    fs::{File, create_dir_all, read_to_string, write},
    io::BufWriter,
    path::{Path, PathBuf},
};

use iced::window::Screenshot;

use super::sections::{ModuleDemo, Tab};

/// One page to photograph: a tab, and for the modules tab the demo shown in it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Shot {
    pub(super) tab: Tab,
    pub(super) module: Option<ModuleDemo>,
}

impl Shot {
    /// Every gallery page in tab order, with the modules tab expanded per demo.
    pub(super) fn all() -> Vec<Self> {
        let mut pages = Vec::new();
        for tab in Tab::ALL {
            if tab == Tab::Modules {
                pages.extend(ModuleDemo::ALL.map(|module| Self {
                    tab,
                    module: Some(module),
                }));
            } else {
                pages.push(Self { tab, module: None });
            }
        }
        pages
    }

    pub(super) fn name(self) -> String {
        let tab = format!("{:?}", self.tab).to_lowercase();
        self.module.map_or_else(
            || format!("{:02}-{tab}", self.tab.index()),
            |module| {
                format!(
                    "{:02}-{tab}-{}",
                    self.tab.index(),
                    format!("{module:?}").to_lowercase()
                )
            },
        )
    }
}

/// Walks every gallery page once, writing a PNG per page.
///
/// Enabled by `KITHARA_GALLERY_CAPTURE=<dir>`; the gallery runs normally when
/// the variable is absent, so the same binary serves both uses.
pub(super) struct Capture {
    dir: PathBuf,
    pending: Vec<Shot>,
    written: Vec<PathBuf>,
}

impl Capture {
    /// Reads the environment and builds the page list, newest page last.
    pub(super) fn requested() -> Option<Self> {
        let dir = PathBuf::from(env::var_os("KITHARA_GALLERY_CAPTURE")?);
        let mut pending = Shot::all();
        pending.reverse();
        Some(Self {
            dir,
            pending,
            written: Vec::new(),
        })
    }

    delegate::delegate! {
        to self.pending {
            #[call(pop)]
            pub(super) fn next(&mut self) -> Option<Shot>;
            #[call(len)]
            pub(super) fn remaining(&self) -> usize;
        }
    }

    /// Encodes one RGBA screenshot and records where it landed.
    pub(super) fn save(&mut self, shot: Shot, screenshot: &Screenshot) -> Result<PathBuf, String> {
        create_dir_all(&self.dir)
            .map_err(|error| format!("create {}: {error}", self.dir.display()))?;
        write_frame(
            &self.dir,
            Frame {
                height: screenshot.size.height,
                scale: f64::from(screenshot.scale_factor),
                width: screenshot.size.width,
            },
        )?;
        let path = self.dir.join(format!("{}.png", shot.name()));
        write_png(
            &path,
            &screenshot.rgba,
            screenshot.size.width,
            screenshot.size.height,
        )?;
        self.written.push(path.clone());
        Ok(path)
    }

    /// One line per page, then the directory — printed when the walk finishes.
    pub(super) fn report(&self) {
        for path in &self.written {
            println!("{}", path.display());
        }
        println!(
            "{} page(s) written to {}",
            self.written.len(),
            self.dir.display()
        );
    }
}

/// Encodes tightly packed RGBA8 as a PNG.
pub(super) fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    let file = File::create(path).map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(rgba))
        .map_err(|error| format!("encode {}: {error}", path.display()))
}

/// The pixel geometry one capture set was taken at. Written beside the pages so
/// another host can be photographed on exactly the same terms, which is the
/// only way a comparison between the two means anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frame {
    pub(super) height: u32,
    pub(super) scale: f64,
    pub(super) width: u32,
}

const FRAME_FILE: &str = "frame.txt";

pub(super) fn write_frame(dir: &Path, frame: Frame) -> Result<(), String> {
    let path = dir.join(FRAME_FILE);
    write(
        &path,
        format!("{} {} {}\n", frame.width, frame.height, frame.scale),
    )
    .map_err(|error| format!("write {}: {error}", path.display()))
}

/// Reads the geometry a capture set was taken at, if one was recorded.
pub(super) fn read_frame(dir: &Path) -> Option<Frame> {
    let text = read_to_string(dir.join(FRAME_FILE)).ok()?;
    let mut parts = text.split_whitespace();
    Some(Frame {
        width: parts.next()?.parse().ok()?,
        height: parts.next()?.parse().ok()?,
        scale: parts.next()?.parse().ok()?,
    })
}
