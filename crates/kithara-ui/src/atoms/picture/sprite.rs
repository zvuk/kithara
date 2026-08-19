use num_traits::cast::AsPrimitive;

use crate::{
    atoms::{button::VisualState, painter::ControlPainter},
    draw::{DrawListBuilder, Image, Rect},
    shaping::TextContext,
};

/// Draws one frame of a sheet, fitted to its box without stretching it.
///
/// The frame arrives already chosen: which one to show is a reading, and this
/// only puts it on the screen. The picture keeps its proportions and sits in
/// the middle of what it was given, because a sheet drawn to a box of another
/// shape would otherwise be squashed differently by every layout that holds it.
pub(crate) struct Sprite;

impl ControlPainter for Sprite {
    type Data = Option<Image>;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        _text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        let Some(image) = data else {
            return;
        };
        list.image(image.clone(), fitted(image, bounds));
    }
}

/// The largest box of the picture's own proportions that the given one holds,
/// centred in it.
fn fitted(image: &Image, bounds: Rect) -> Rect {
    let (natural_w, natural_h): (f32, f32) = (image.width().as_(), image.height().as_());
    if natural_w <= 0.0 || natural_h <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return bounds;
    }
    let scale = (bounds.w / natural_w).min(bounds.h / natural_h);
    let (w, h) = (natural_w * scale, natural_h * scale);
    Rect {
        h,
        w,
        x: bounds.x + (bounds.w - w) / 2.0,
        y: bounds.y + (bounds.h - h) / 2.0,
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::fitted;
    use crate::draw::{Image, ImageId, Rect};

    const BOX: Rect = Rect {
        h: 40.0,
        w: 100.0,
        x: 10.0,
        y: 20.0,
    };

    #[kithara::test]
    fn a_square_picture_in_a_wide_box_takes_the_height() {
        assert_eq!(fitted(&square(), BOX).h, BOX.h);
    }

    #[kithara::test]
    fn a_square_picture_in_a_wide_box_stays_square() {
        let placed = fitted(&square(), BOX);

        assert_eq!(placed.w, placed.h);
    }

    #[kithara::test]
    fn a_fitted_picture_is_centred_in_what_it_was_given() {
        let placed = fitted(&square(), BOX);

        assert_eq!(
            placed.x + placed.w / 2.0,
            BOX.x + BOX.w / 2.0,
            "the picture's centre is the box's centre"
        );
    }

    fn square() -> Image {
        Image::pixels(ImageId::new("test/square"), 4, 4, Arc::from(vec![0_u8; 64]))
            .unwrap_or_else(|| panic!("a four by four block of pixels is a picture"))
    }
}
