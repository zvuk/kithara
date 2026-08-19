use std::sync::LazyLock;

use kithara_platform::sync::Arc;
use png::{ColorType, Decoder, Transformations};

use crate::{
    builtin,
    draw::{Image, ImageId},
};

/// The name every sheet this toolkit ships is asked for by.
const SPINNER: &str = "spinner";

/// One of the sheets the toolkit ships, cut on first use.
///
/// A document names a sheet rather than carrying pixels, and this is the whole
/// list of names it may use. A name nothing answers draws nothing, which is
/// what an unbound control does everywhere else.
#[must_use]
pub fn builtin_sheet(name: &str) -> Option<&'static Sheet> {
    static SHEET: LazyLock<Option<Sheet>> = LazyLock::new(|| {
        Sheet::cut(SPINNER, builtin::SPINNER_SHEET, 8, 1)
            .inspect_err(|error| tracing::error!(%error, "the built-in sprite sheet did not cut"))
            .ok()
    });
    (name == SPINNER).then(|| SHEET.as_ref()).flatten()
}

/// What a sheet could not be read as.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SheetError {
    #[error("sprite sheet {name:?} is not a readable PNG: {source}")]
    Decode {
        name: String,
        source: png::DecodingError,
    },
    #[error("sprite sheet {name:?} is {colour:?}, and only RGB and RGBA are cut into frames")]
    Colour { name: String, colour: ColorType },
    #[error(
        "sprite sheet {name:?} is {width}x{height}, which {columns}x{rows} frames do not divide"
    )]
    Grid {
        name: String,
        width: u32,
        height: u32,
        columns: u32,
        rows: u32,
    },
}

/// A grid of pictures cut out of one image, in reading order.
///
/// The cutting happens once, when the sheet is read: a frame is its own picture
/// with its own identity, so a rasteriser uploads each one once and a drawing
/// asks for it by index rather than carrying a source rectangle the seam would
/// then have to describe to two toolkits that spell it differently.
#[derive(Clone, Debug)]
pub struct Sheet {
    frames: Vec<Image>,
}

impl Sheet {
    /// Reads a PNG and cuts it into `columns` by `rows` frames of equal size.
    ///
    /// # Errors
    /// Returns why the sheet could not be cut: unreadable, a colour type with
    /// no straight RGBA reading, or a grid that does not divide the image.
    pub fn cut(name: &str, png: &[u8], columns: u32, rows: u32) -> Result<Self, SheetError> {
        let (info, pixels) = read(name, png)?;
        if columns == 0 || rows == 0 || info.width % columns != 0 || info.height % rows != 0 {
            return Err(SheetError::Grid {
                columns,
                height: info.height,
                name: name.to_owned(),
                rows,
                width: info.width,
            });
        }
        let (frame_w, frame_h) = (info.width / columns, info.height / rows);
        let frames = (0..rows)
            .flat_map(|row| (0..columns).map(move |column| (row, column)))
            .enumerate()
            .filter_map(|(index, (row, column))| {
                let id = ImageId::new(&format!("{name}#{index}"));
                let cut = crop(
                    &pixels,
                    info.width,
                    (column * frame_w, row * frame_h),
                    (frame_w, frame_h),
                );
                Image::pixels(id, frame_w, frame_h, Arc::from(cut))
            })
            .collect::<Vec<_>>();
        Ok(Self { frames })
    }

    /// The picture at one index, wrapping so a running index keeps drawing.
    #[must_use]
    pub fn frame(&self, index: usize) -> Option<&Image> {
        self.frames.get(index.checked_rem(self.frames.len())?)
    }

    delegate::delegate! {
        to self.frames {
            #[must_use]
            pub fn len(&self) -> usize;
            #[must_use]
            pub fn is_empty(&self) -> bool;
        }
    }
}

/// The sheet as straight RGBA8, whatever the file spelled it as.
fn read(name: &str, png: &[u8]) -> Result<(png::OutputInfo, Vec<u8>), SheetError> {
    let mut decoder = Decoder::new(png);
    decoder.set_transformations(Transformations::normalize_to_color8() | Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|source| SheetError::Decode {
        name: name.to_owned(),
        source,
    })?;
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|source| SheetError::Decode {
            name: name.to_owned(),
            source,
        })?;
    if info.color_type != ColorType::Rgba {
        return Err(SheetError::Colour {
            colour: info.color_type,
            name: name.to_owned(),
        });
    }
    pixels.truncate(info.buffer_size());
    Ok((info, pixels))
}

/// One frame's pixels, lifted row by row out of the sheet.
fn crop(sheet: &[u8], sheet_width: u32, at: (u32, u32), size: (u32, u32)) -> Vec<u8> {
    let stride = sheet_width as usize * 4;
    let (x, y) = (at.0 as usize * 4, at.1 as usize);
    let width = size.0 as usize * 4;
    (0..size.1 as usize)
        .flat_map(|row| {
            let start = (y + row) * stride + x;
            sheet.get(start..start + width).unwrap_or_default()
        })
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Sheet, builtin_sheet};
    use crate::{builtin, draw::Image};

    #[kithara::test]
    fn the_builtin_sheet_cuts_into_the_frames_it_declares() {
        let sheet = Sheet::cut("spinner", builtin::SPINNER_SHEET, 8, 1)
            .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

        assert_eq!(sheet.len(), 8);
    }

    #[kithara::test]
    fn every_frame_of_the_builtin_sheet_is_square() {
        let sheet = Sheet::cut("spinner", builtin::SPINNER_SHEET, 8, 1)
            .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

        for index in 0..sheet.len() {
            let Some(frame) = sheet.frame(index) else {
                panic!("frame {index} must be cut");
            };
            assert_eq!(frame.width(), frame.height(), "frame {index}");
        }
    }

    /// Two frames of one sheet are two pictures, not one drawn twice: a
    /// rasteriser keyed on identity would otherwise show the first everywhere.
    #[kithara::test]
    fn two_frames_of_one_sheet_are_two_pictures() {
        let sheet = Sheet::cut("spinner", builtin::SPINNER_SHEET, 8, 1)
            .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

        assert_ne!(sheet.frame(0).map(Image::id), sheet.frame(1).map(Image::id));
    }

    #[kithara::test]
    fn frames_of_one_sheet_differ_in_their_pixels() {
        let sheet = Sheet::cut("spinner", builtin::SPINNER_SHEET, 8, 1)
            .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

        assert_ne!(
            sheet.frame(0).and_then(Image::rgba),
            sheet.frame(4).and_then(Image::rgba)
        );
    }

    #[kithara::test]
    fn a_running_index_wraps_back_to_the_first_frame() {
        let sheet = Sheet::cut("spinner", builtin::SPINNER_SHEET, 8, 1)
            .unwrap_or_else(|error| panic!("the embedded sheet must cut: {error}"));

        assert_eq!(sheet.frame(8).map(Image::id), sheet.frame(0).map(Image::id));
    }

    #[kithara::test]
    fn a_grid_that_does_not_divide_the_sheet_is_refused() {
        assert!(Sheet::cut("spinner", builtin::SPINNER_SHEET, 7, 1).is_err());
    }

    #[kithara::test]
    fn something_that_is_not_a_png_is_refused() {
        assert!(Sheet::cut("nonsense", b"not a png", 1, 1).is_err());
    }

    #[kithara::test]
    fn the_sheet_the_toolkit_ships_is_reachable_by_name() {
        assert!(builtin_sheet("spinner").is_some());
    }

    /// A document naming a sheet nothing ships draws nothing, rather than
    /// standing in for it with a sheet it did not ask for.
    #[kithara::test]
    fn a_sheet_the_toolkit_does_not_ship_answers_nothing() {
        assert!(builtin_sheet("no-such-sheet").is_none());
    }

    /// The sheet is cut once, so a frame drawn on one screen and the same frame
    /// drawn on the next are one picture to whatever uploads it.
    #[kithara::test]
    fn asking_twice_gives_back_the_same_cut() {
        let (first, again) = (builtin_sheet("spinner"), builtin_sheet("spinner"));

        assert_eq!(
            first.and_then(|sheet| sheet.frame(0)).map(Image::rgba),
            again.and_then(|sheet| sheet.frame(0)).map(Image::rgba)
        );
    }
}
