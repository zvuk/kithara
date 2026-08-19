use std::cell::RefCell;

#[cfg(feature = "iced")]
use iced::Element;

use crate::{
    draw::{DrawList, DrawListBuilder, Pt, Rect, Rgba, Transform},
    interact::CursorShape,
    render::{HostLayer, LayerHit, Skin, WindowCommand, WindowLayerProgram},
    shaping::{TextContext, TextResources},
    skin::TextRoleSkin,
    solve::{Length, Size},
};

#[derive(bon::Builder)]
pub(crate) struct TitleBar<'label, 'skin> {
    skin: &'skin Skin,
    label: &'label str,
}

#[cfg(feature = "iced")]
impl<'a> crate::render::Widget<'a> for TitleBar<'_, '_> {
    fn view(self) -> Element<'a, crate::render::UiEvent> {
        crate::render::window_layer(TitleProgram::new(self.label, self.skin))
    }
}

pub(crate) struct TitleProgram {
    color: Rgba,
    label: String,
    padding_x: f32,
    resources: TextResources,
    role: TextRoleSkin,
}

impl TitleProgram {
    pub(crate) fn new(label: &str, skin: &Skin) -> Self {
        let metrics = skin.window;
        Self {
            color: skin.rgba(metrics.titlebar_text.color),
            label: label.to_owned(),
            padding_x: metrics.titlebar_padding_x,
            resources: skin.text_resources().clone(),
            role: metrics.titlebar_text,
        }
    }

    fn paint(&self, text: &mut TextContext, bounds: Rect) -> DrawList {
        let mut builder = DrawListBuilder::default();
        if self.label.is_empty() {
            return builder.finish();
        }
        let max_width = (bounds.w - self.padding_x * 2.0).max(0.0);
        let run = text.shape(&self.label, self.role, Some(max_width));
        builder.text(
            &run,
            &self.label,
            Transform::translate(Pt {
                x: bounds.x + self.padding_x,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            self.color,
        );
        builder.finish()
    }
}

#[derive(Default)]
pub(crate) struct TitleState {
    text: RefCell<Option<TextContext>>,
}

impl WindowLayerProgram for TitleProgram {
    type State = TitleState;

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layer(
        &self,
        state: &TitleState,
        bounds: Rect,
        _pointer: Option<Pt>,
    ) -> HostLayer<WindowCommand> {
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| TextContext::from(&self.resources));
        let list = self.paint(
            text,
            Rect {
                h: bounds.h,
                w: bounds.w,
                x: 0.0,
                y: 0.0,
            },
        );
        HostLayer::new(
            bounds,
            list,
            vec![LayerHit::new(
                bounds,
                CursorShape::None,
                WindowCommand::Drag,
            )],
        )
    }

    fn hit_layer(&self, _state: &TitleState, bounds: Rect) -> HostLayer<WindowCommand> {
        HostLayer::new(
            bounds,
            DrawList::default(),
            vec![LayerHit::new(
                bounds,
                CursorShape::None,
                WindowCommand::Drag,
            )],
        )
    }

    fn resources(&self) -> Option<&TextResources> {
        Some(&self.resources)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::DrawCmd,
        interact::{Input, Outcome, PointerPhase, mouse as mouse_input},
    };

    fn pointer_down() -> Input<'static> {
        Input::Pointer(mouse_input(PointerPhase::Down, None))
    }

    #[kithara::test]
    fn title_text_is_retained_as_a_draw_command() {
        let skin = builtin::skin();
        let program = TitleProgram::new("KITHARA", skin);
        let bounds = Rect {
            h: 32.0,
            w: 180.0,
            x: 17.0,
            y: 9.0,
        };
        let layer = program.layer(&TitleState::default(), bounds, None);

        assert!(matches!(
            layer.draw().commands(),
            [DrawCmd::Text {
                content,
                transform,
                color,
                ..
            }] if content == "KITHARA"
                && transform.dx == skin.window.titlebar_padding_x
                && *color == skin.rgba(skin.window.titlebar_text.color)
        ));
        assert_eq!(layer.bounds(), bounds);
        assert_eq!(layer.hits()[0].area(), bounds);
    }

    #[kithara::test]
    fn a_titlebar_press_uses_the_default_layer_update() {
        let program = TitleProgram::new("KITHARA", builtin::skin());
        let bounds = Rect {
            h: 32.0,
            w: 180.0,
            x: 17.0,
            y: 9.0,
        };
        let pointer = Some(Pt { x: 90.0, y: 16.0 });
        let mut state = TitleState::default();
        let layer = program.hit_layer(&state, bounds);
        let (outcome, redraw) = program.update(&mut state, pointer_down(), &layer, pointer);

        assert_eq!(outcome, Outcome::set(WindowCommand::Drag));
        assert!(!redraw);
        assert!(layer.draw().commands().is_empty());
        assert_eq!(layer.cursor_at(pointer), CursorShape::None);
    }
}
