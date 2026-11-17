use crate::{
    atoms::icon::mark::Marked,
    draw::{DrawListBuilder, Rect, Rgba},
    hosts::icons::Mark,
    shaping::TextContext,
};

/// One icon on its own, centred in the box it was given.
///
/// Unlike the icon inside a button or a rail item, this one is the whole
/// control: nothing sits beside it, so it has the middle of the box rather than
/// a left edge to start from.
#[derive(Clone, PartialEq, kithara_derive::ControlPainter)]
#[control_painter(
    data = GlyphData,
    draw = self.paint(list, text, data, bounds)
)]
pub(crate) struct Glyph {
    size: f32,
}

/// What a glyph is handed each frame: the art and colour it draws right now.
#[derive(Clone, PartialEq)]
pub(crate) struct GlyphData {
    pub(crate) mark: Mark,
    pub(crate) color: Rgba,
}

impl Glyph {
    pub(crate) const fn new(size: f32) -> Self {
        Self { size }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &GlyphData,
        bounds: Rect,
    ) {
        Marked::new(data.mark, self.size).centred(list, text, bounds, data.color);
    }
}
