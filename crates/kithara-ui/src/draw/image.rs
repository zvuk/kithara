use kithara_platform::sync::Arc;

/// Stable identity of an image.
///
/// Two draws of the same identity are the same picture, which is what lets a
/// rasteriser keep one texture for it instead of uploading the pixels again
/// every frame.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ImageId(Arc<str>);

impl ImageId {
    /// Creates an image identity.
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self(Arc::from(id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One picture the list draws: an identity, a size, and where its pixels are.
///
/// A picture drawn from pixels carries them along rather than being looked up,
/// so nothing has to hand a registry to every backend on the way. They are
/// shared, not copied — the clone a command carries is a reference count.
/// RGBA8 is the one layout both rasterisers take without conversion, so the
/// seam names it rather than a format neither can use directly.
///
/// A picture rendered on the device carries no pixels at all: the producer
/// bound a texture to its identity, and the backend that owns that binding is
/// the only one that can draw it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Image {
    height: u32,
    id: ImageId,
    rgba: Option<Arc<[u8]>>,
    width: u32,
}

impl Image {
    /// Creates a picture from pixels, or nothing when they do not fill the size.
    ///
    /// A short buffer is a defect in whatever produced it, not something to
    /// draw part of: every rasteriser below reads `width * height * 4` bytes.
    #[must_use]
    pub fn pixels(id: ImageId, width: u32, height: u32, rgba: Arc<[u8]>) -> Option<Self> {
        let wanted = usize::try_from(width).ok()?.checked_mul(4)?;
        let wanted = wanted.checked_mul(usize::try_from(height).ok()?)?;
        if wanted == 0 || rgba.len() != wanted {
            return None;
        }
        Some(Self {
            height,
            id,
            rgba: Some(rgba),
            width,
        })
    }

    /// Creates a picture whose pixels live on the device under this identity.
    #[must_use]
    pub const fn external(id: ImageId, width: u32, height: u32) -> Self {
        Self {
            height,
            id,
            rgba: None,
            width,
        }
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub const fn id(&self) -> &ImageId {
        &self.id
    }

    /// The pixels, or nothing when they live on the device.
    #[must_use]
    pub fn rgba(&self) -> Option<&Arc<[u8]>> {
        self.rgba.as_ref()
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::{Image, ImageId};

    #[kithara::test]
    fn a_picture_whose_pixels_fill_its_size_is_drawable() {
        let image = Image::pixels(ImageId::new("sheet"), 2, 1, Arc::from(vec![0_u8; 8]));

        assert!(image.is_some());
    }

    #[kithara::test]
    fn a_picture_shorter_than_its_size_is_not_a_picture() {
        assert_eq!(
            Image::pixels(ImageId::new("sheet"), 2, 1, Arc::from(vec![0_u8; 7])),
            None
        );
    }

    #[kithara::test]
    fn a_picture_longer_than_its_size_is_not_a_picture() {
        assert_eq!(
            Image::pixels(ImageId::new("sheet"), 2, 1, Arc::from(vec![0_u8; 9])),
            None
        );
    }

    #[kithara::test]
    fn a_picture_with_no_area_is_not_a_picture() {
        assert_eq!(
            Image::pixels(ImageId::new("sheet"), 0, 4, Arc::from(Vec::new())),
            None
        );
    }

    #[kithara::test]
    fn a_picture_on_the_device_carries_no_pixels() {
        assert_eq!(
            Image::external(ImageId::new("shader/field"), 8, 8).rgba(),
            None
        );
    }
}
