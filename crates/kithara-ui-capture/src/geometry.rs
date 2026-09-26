use std::{
    fmt::Display,
    fs::{File, read_to_string, write},
    io::{BufWriter, Write},
    path::Path,
};

use png::{BitDepth, ColorType, Encoder, Writer};

/// The pixel geometry one capture set was taken at.
///
/// Written beside the pages so another host can be photographed on exactly the
/// same terms, which is the only way a comparison between the two means
/// anything. A caller builds one, so its fields stay open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub scale: f64,
    pub height: u32,
    pub width: u32,
}

/// The name the first capture set was written with, and every set since.
const GEOMETRY_FILE: &str = "frame.txt";

/// Records the geometry a set was photographed at, beside the set.
///
/// # Errors
/// Fails when the file cannot be written.
pub fn write_geometry(dir: &Path, geometry: Geometry) -> Result<(), String> {
    let path = dir.join(GEOMETRY_FILE);
    write(
        &path,
        format!(
            "{} {} {}\n",
            geometry.width, geometry.height, geometry.scale
        ),
    )
    .map_err(|error| format!("write {}: {error}", path.display()))
}

/// Reads the geometry a capture set was taken at, if one was recorded.
#[must_use]
pub fn read_geometry(dir: &Path) -> Option<Geometry> {
    let text = read_to_string(dir.join(GEOMETRY_FILE)).ok()?;
    let mut parts = text.split_whitespace();
    Some(Geometry {
        width: parts.next()?.parse().ok()?,
        height: parts.next()?.parse().ok()?,
        scale: parts.next()?.parse().ok()?,
    })
}

/// Encodes tightly packed RGBA8 rows as a PNG.
///
/// The rows are handed to the encoder as they come rather than gathered into a
/// buffer first, so a picture cut out of a photograph costs no storage beyond
/// the photograph it was cut from.
///
/// # Errors
/// Fails when the file cannot be written or the rows do not fill the frame.
pub fn write_png<'row, Rows>(path: &Path, width: u32, height: u32, rows: Rows) -> Result<(), String>
where
    Rows: Iterator<Item = &'row [u8]>,
{
    let file = File::create(path).map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut encoder = Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .and_then(Writer::into_stream_writer)
        .map_err(|error| failed(path, error))?;
    for row in rows {
        writer.write_all(row).map_err(|error| failed(path, error))?;
    }
    writer.finish().map_err(|error| failed(path, error))
}

/// What a picture that could not be encoded says, so every step of one write
/// names the same file the same way.
fn failed<Error: Display>(path: &Path, error: Error) -> String {
    format!("encode {}: {error}", path.display())
}
