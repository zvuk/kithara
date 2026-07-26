use std::{fs, io, path::Path};

const HEADER_LEN: usize = 54;
const DPI_PPM: u32 = 2835;

/// Write an RGBA screenshot buffer as a 32-bit bottom-up BMP.
///
/// # Errors
///
/// Returns [`io::Error`] when the buffer is shorter than `width * height` or the
/// file cannot be written.
pub fn write(path: &Path, width: u32, height: u32, rgba: &[u8]) -> io::Result<()> {
    let row = width as usize * 4;
    let pixels = row * height as usize;
    if rgba.len() < pixels {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short pixel buffer",
        ));
    }
    let file_size = u32::try_from(HEADER_LEN + pixels).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(file_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(HEADER_LEN).unwrap_or_default().to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(pixels).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&DPI_PPM.to_le_bytes());
    out.extend_from_slice(&DPI_PPM.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..height as usize).rev() {
        for px in rgba[y * row..y * row + row].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
    fs::write(path, out)
}
