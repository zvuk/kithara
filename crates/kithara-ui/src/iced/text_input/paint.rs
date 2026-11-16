use std::cell::RefCell;

use iced::{
    Rectangle, Renderer,
    widget::canvas::{Frame, Geometry},
};

use crate::{
    atoms::{
        search::{input_bounds, paint::paint_shaped},
        text_input::TextInputPaint,
    },
    backends::finish,
    draw::{DrawListBuilder, Rect},
    engine::TextInputSnapshot,
    interact::TextInputLayout,
    render::Skin,
    shaping::TextContext,
};

/// A search face whose query is shaped once, when the face is built.
pub(super) struct SearchPaint<'a> {
    input: TextInputPaint<'a>,
    skin: &'a Skin,
    text: RefCell<TextContext>,
}

impl<'a> SearchPaint<'a> {
    pub(super) fn new(query: &str, skin: &'a Skin) -> Self {
        let mut text = TextContext::from(skin.text_resources.as_ref());
        Self {
            input: TextInputPaint::with_context(query.to_owned().into(), skin, &mut text),
            skin,
            text: RefCell::new(text),
        }
    }

    pub(super) fn layout(&self) -> TextInputLayout {
        self.input.layout()
    }

    pub(super) fn input_bounds(&self, bounds: Rectangle) -> Rectangle {
        let input = input_bounds(bounds.into(), self.skin);
        Rectangle {
            x: input.x,
            y: input.y,
            width: input.w,
            height: input.h,
        }
    }

    pub(super) fn geometry(
        &self,
        snapshot: &TextInputSnapshot,
        renderer: &Renderer,
        bounds: Rectangle,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let mut list = DrawListBuilder::default();
        paint_shaped(
            &mut list,
            &mut self.text.borrow_mut(),
            Rect {
                h: bounds.height,
                w: bounds.width,
                x: 0.0,
                y: 0.0,
            },
            &self.input,
            self.skin,
            snapshot,
        );
        crate::backends::replay_ordered(
            &list.finish(),
            &mut frame,
            self.skin.text_resources.as_ref(),
        );
        vec![finish(frame.into_geometry())]
    }
}
