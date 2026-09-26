use kithara_platform::sync::Arc;
use png::{ColorType, Decoder, Transformations};

use crate::draw::{Image, ImageId};

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
#[derive(Clone, Debug, Eq, PartialEq)]
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
                rows,
                height: info.height,
                name: name.to_owned(),
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
                Image::pixels(id, frame_w, frame_h, cut)
            })
            .collect::<Vec<Image>>();
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
        source,
        name: name.to_owned(),
    })?;
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|source| SheetError::Decode {
            source,
            name: name.to_owned(),
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
fn crop(sheet: &[u8], sheet_width: u32, at: (u32, u32), size: (u32, u32)) -> Arc<[u8]> {
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
