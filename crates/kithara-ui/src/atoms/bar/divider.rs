use crate::{
    draw::{DrawListBuilder, Rect, Rgba},
    render::Skin,
};

/// A hairline separating two runs of a bar: one filled rectangle, the whole of
/// the box it was given.
pub(crate) struct Divider {
    color: Rgba,
}

impl Divider {
    pub(crate) fn new(skin: &Skin) -> Self {
        Self {
            color: skin.rgba(skin.divider.color),
        }
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, bounds: Rect) {
        list.fill_rect(bounds, self.color);
    }
}
