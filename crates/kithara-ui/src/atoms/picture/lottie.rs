use num_traits::cast::AsPrimitive;

use crate::{
    atoms::{button::VisualState, painter::ControlPainter},
    draw::{DrawListBuilder, Rect, Transform},
    lottie::{Artwork, emit::emit},
    shaping::TextContext,
};

/// Which artwork stands at which of its own frames.
///
/// The artwork travels with the frame rather than with the painter, because
/// which artwork a control shows is resolved from the document and a name the
/// toolkit does not ship resolves to nothing at all.
#[derive(Clone, Copy, Default)]
pub(crate) struct Standing {
    pub(crate) artwork: Option<&'static Artwork>,
    pub(crate) frame: f64,
}

/// Draws one frame of an artwork, fitted to its box without stretching it.
///
/// The frame arrives already chosen: which one to show is a reading, and this
/// only puts it on the screen. The artwork keeps the proportions it was
/// authored in and sits in the middle of what it was given, because a drawing
/// fitted to a box of another shape would otherwise be squashed differently by
/// every layout that holds it.
pub(crate) struct Lottie;

impl ControlPainter for Lottie {
    type Data = Standing;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        _text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        let Some(artwork) = data.artwork else {
            return;
        };
        let frame = data.frame;
        list.transformed(fitted(artwork.size(), bounds), |list| {
            // An artwork that half draws is a picture nobody authored, so a
            // refusal leaves the box as it found it rather than part-drawn.
            if let Err(error) = emit(artwork.composition(), frame, list) {
                tracing::error!(%error, "artwork not drawn");
            }
        });
    }
}

/// Puts the artwork's own box in the middle of the given one at the largest
/// scale that fits, as one transform the neutral list resolves into the points.
fn fitted((natural_w, natural_h): (f64, f64), bounds: Rect) -> Transform {
    let (natural_w, natural_h): (f32, f32) = (natural_w.as_(), natural_h.as_());
    if natural_w <= 0.0 || natural_h <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return Transform::IDENTITY;
    }
    let scale = (bounds.w / natural_w).min(bounds.h / natural_h);
    let (w, h) = (natural_w * scale, natural_h * scale);
    Transform {
        xx: scale,
        xy: 0.0,
        yx: 0.0,
        yy: scale,
        dx: bounds.x + (bounds.w - w) / 2.0,
        dy: bounds.y + (bounds.h - h) / 2.0,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::fitted;
    use crate::draw::{Pt, Rect, Transform};

    const BOX: Rect = Rect {
        h: 40.0,
        w: 100.0,
        x: 10.0,
        y: 20.0,
    };

    /// A square artwork of 200 in the wide box above, which is the case a
    /// stretch would show.
    fn placed() -> Transform {
        fitted((200.0, 200.0), BOX)
    }

    #[kithara::test]
    fn a_square_artwork_in_a_wide_box_takes_the_height() {
        let placed = placed();
        let corner = placed.apply(Pt { x: 200.0, y: 200.0 });

        assert_eq!(corner.y - placed.dy, BOX.h);
    }

    #[kithara::test]
    fn a_square_artwork_in_a_wide_box_stays_square() {
        let placed = placed();
        let corner = placed.apply(Pt { x: 200.0, y: 200.0 });

        assert_eq!(corner.x - placed.dx, corner.y - placed.dy);
    }

    #[kithara::test]
    fn a_fitted_artwork_is_centred_in_what_it_was_given() {
        let placed = placed();
        let middle = placed.apply(Pt { x: 100.0, y: 100.0 });

        assert_eq!(middle.x, BOX.x + BOX.w / 2.0);
    }
}
