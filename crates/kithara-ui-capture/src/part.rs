use std::{
    fmt::Display,
    fs::create_dir_all,
    ops::Range,
    path::{Path, PathBuf},
};

use kurbo::Rect;
use num_traits::cast::AsPrimitive;

use super::{Geometry, Stage, write_png};

/// A rectangle of one photograph, in that photograph's own pixels.
///
/// A caller builds one to say which part of a frame a picture is cut from, so
/// its fields stay open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Region {
    pub height: u32,
    pub width: u32,
    pub x: u32,
    pub y: u32,
}

impl Region {
    /// The pixels a rectangle laid out in points covers in a photograph taken
    /// at this scale.
    ///
    /// # Errors
    /// Refuses a rectangle that starts before the frame does, or that covers
    /// nothing: rounding either one into the frame would photograph somewhere
    /// else and report it as the control that was asked for.
    pub fn of(rect: Rect, scale: f64) -> Result<Self, String> {
        if rect.x0 < 0.0 || rect.y0 < 0.0 {
            return Err(format!(
                "a control at {},{} is laid out before the frame begins",
                rect.x0, rect.y0
            ));
        }
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(format!(
                "a control of {}x{} points covers nothing",
                rect.width(),
                rect.height()
            ));
        }
        let pixels = |points: f64| -> u32 { (points * scale).round().as_() };
        Ok(Self {
            height: pixels(rect.height()),
            width: pixels(rect.width()),
            x: pixels(rect.x0),
            y: pixels(rect.y0),
        })
    }

    /// Where each row of this region sits in the pixels of that frame.
    ///
    /// # Errors
    /// Refuses a region that covers no pixel or that leaves the frame, and a
    /// frame the pixels do not fill. A region is never clipped to fit: a
    /// picture smaller than the control that was asked for is not a picture of
    /// it, and a run that photographed one would report it as one that did.
    fn rows(
        self,
        frame: Geometry,
        pixels: &[u8],
    ) -> Result<impl Iterator<Item = Range<usize>>, String> {
        /// How many bytes one pixel of a photograph carries.
        const CHANNELS: usize = 4;

        if self.width == 0 || self.height == 0 {
            return Err(format!(
                "a region of {}x{} pixels photographs nothing",
                self.width, self.height
            ));
        }
        let corner = self
            .x
            .checked_add(self.width)
            .zip(self.y.checked_add(self.height));
        let Some((_, bottom)) =
            corner.filter(|&(right, bottom)| right <= frame.width && bottom <= frame.height)
        else {
            return Err(format!(
                "a region of {}x{} at {},{} leaves the {}x{} frame",
                self.width, self.height, self.x, self.y, frame.width, frame.height
            ));
        };
        let stride = AsPrimitive::<usize>::as_(frame.width) * CHANNELS;
        let filled = stride * AsPrimitive::<usize>::as_(frame.height);
        if pixels.len() != filled {
            return Err(format!(
                "a {}x{} frame is {filled} bytes, got {}",
                frame.width,
                frame.height,
                pixels.len()
            ));
        }
        let width = AsPrimitive::<usize>::as_(self.width) * CHANNELS;
        let start = AsPrimitive::<usize>::as_(self.x) * CHANNELS;
        let rows = AsPrimitive::<usize>::as_(self.y)..AsPrimitive::<usize>::as_(bottom);
        Ok(rows.map(move |row| {
            let from = row * stride + start;
            from..from + width
        }))
    }
}

/// What a stage answers when it knows where the controls it drew ended up.
///
/// Only a host that keeps its tree between frames can say: an immediate host
/// builds and forgets the tree inside one draw, and never named what it built.
/// It is a trait of its own rather than a method on [`Stage`] for exactly that
/// reason - a host with no answer should not compile against the question.
pub trait Locate {
    /// Where the control at this document path was laid out, in the logical
    /// points the host lays out in, or `None` when the open page draws no such
    /// control.
    fn locate(&self, path: &str) -> Option<Rect>;
}

/// Photographs one control of one page into a directory, answering with the
/// file written.
///
/// One picture rather than a set: a set is photographed on one geometry for
/// every page in it, and a control is laid out to a different rectangle on
/// every page that draws it, so a set of controls has no frame to record.
///
/// # Errors
/// Fails when the directory cannot be made, when the stage cannot open or draw
/// the page, when the page draws no control at that path, or when the region
/// the control was laid out in cannot be cut out of the photograph.
pub fn shoot_part<S>(
    stage: &mut S,
    page: &S::Page,
    path: &str,
    dir: &Path,
) -> Result<PathBuf, String>
where
    S: Stage + Locate,
    S::Page: Display,
{
    create_dir_all(dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    stage.turn(page)?;
    let frame = stage.geometry();
    let rect = stage
        .locate(path)
        .ok_or_else(|| format!("{page} draws no control at {path}"))?;
    let region = Region::of(rect, frame.scale)?;
    let file = dir.join(part_file(page, path));
    let pixels = stage.shoot()?;
    let rows = region.rows(frame, pixels)?;
    write_png(
        &file,
        region.width,
        region.height,
        rows.map(|row| &pixels[row]),
    )?;
    Ok(file)
}

/// Where a photograph of one control lands: the page it was drawn on and the
/// control's own path, whose separators a file name cannot carry.
pub fn part_file<Page: Display>(page: &Page, path: &str) -> String {
    let control: String = path
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() {
                char
            } else {
                '-'
            }
        })
        .collect();
    format!("{page}-{control}.png")
}
