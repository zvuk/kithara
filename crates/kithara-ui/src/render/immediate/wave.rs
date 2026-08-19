use std::cell::RefCell;

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme,
    keyboard::{Event as KeyboardEvent, Modifiers},
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry},
};
use kithara_platform::time::Instant;

use crate::{
    atoms::wave::{
        face::{Drawn, Wave as Face},
        zoom_math::{window_bounds, x_to_norm, zoom_for_wheel},
    },
    backends::replay_ordered,
    draw::{DrawListBuilder, Rect},
    interact::{
        CursorShape, Hover, Input, iced as iced_interact,
        recognizers::{Scalar, ScalarState, Track},
    },
    module::WaveStyle,
    render::{Skin, UiEvent, Widget, controls::snapped, scalar, scalar_child},
    shaping::TextContext,
};

/// The waveform with the gesture only the immediate host recognises: shift to
/// mark a loop, the wheel to zoom, a drag to scrub. The picture is the shared
/// painter's; this adds nothing to it.
#[derive(bon::Builder)]
pub(crate) struct MiniWave<'path, 'skin> {
    skin: &'skin Skin,
    path: &'path str,
    data: Drawn,
    style: WaveStyle,
}

impl<'a, 'skin: 'a> Widget<'a> for MiniWave<'_, 'skin> {
    fn view(self) -> Element<'a, UiEvent> {
        let painter = Face::new(self.style, self.skin);
        let hero = painter.hero();
        let drag = Scalar::builder()
            .track(if hero {
                Track::RelativeHorizontal {
                    scale: self.data.zoom,
                    value: self.data.progress,
                }
            } else {
                Track::HorizontalClick
            })
            .hover(Hover::new(if hero {
                CursorShape::Grab
            } else {
                CursorShape::Pointer
            }))
            .build();
        Canvas::new(MiniWaveCanvas {
            data: self.data,
            drag,
            painter,
            path: self.path.to_owned(),
            skin: self.skin,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct MiniWaveCanvas<'skin> {
    data: Drawn,
    drag: Scalar,
    painter: Face,
    path: String,
    skin: &'skin Skin,
}

#[derive(Default)]
struct MiniWaveState {
    modifiers: Modifiers,
    loop_start: Option<f32>,
    drag: ScalarState,
    text: RefCell<Option<TextContext>>,
}

impl canvas::Program<UiEvent> for MiniWaveCanvas<'_> {
    type State = MiniWaveState;

    fn draw(
        &self,
        state: &MiniWaveState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let bounds = snapped(bounds);
        let show_overlay = !cursor.is_over(iced_bounds(self.painter.overlay_bounds(
            &self.data,
            Rect {
                h: bounds.height,
                w: bounds.width,
                x: bounds.x,
                y: bounds.y,
            },
        )));
        let bounds = local_bounds(&bounds);
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.skin.text_resources().into());
        let mut list = DrawListBuilder::default();
        self.painter
            .paint(&mut list, text, &self.data, bounds, show_overlay);
        let mut frame = Frame::new(renderer, Size::new(bounds.w, bounds.h));
        replay_ordered(&list.finish(), &mut frame, self.skin.text_resources());
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &MiniWaveState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if self.data.has_waveform() {
            self.drag
                .cursor(&state.drag, &iced_interact::hit(bounds, cursor))
                .into()
        } else {
            mouse::Interaction::default()
        }
    }

    fn update(
        &self,
        state: &mut MiniWaveState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        if let Event::Keyboard(KeyboardEvent::ModifiersChanged(modifiers)) = event {
            state.modifiers = *modifiers;
            return None;
        }
        if !self.data.has_waveform() {
            return None;
        }
        if self.painter.hero() {
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                    if state.modifiers.shift() && cursor.is_over(bounds) =>
                {
                    let start = self.track_position(bounds, cursor)?;
                    state.loop_start = Some(start);
                    return Some(scalar_child(&self.path, "loop_start", f64::from(start)));
                }
                Event::Mouse(mouse::Event::CursorMoved { .. }) if state.loop_start.is_some() => {
                    let end = self.track_position(bounds, cursor)?;
                    return Some(scalar_child(&self.path, "loop_end", f64::from(end)));
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                    if state.loop_start.take().is_some() =>
                {
                    return Some(Action::capture());
                }
                _ => {}
            }
        }
        if self.painter.hero()
            && let Some(Input::Wheel(scroll)) = iced_interact::input(event)
            && cursor.is_over(bounds)
        {
            let zoom = zoom_for_wheel(self.data.zoom, scroll.y());
            return Some(scalar_child(&self.path, "zoom", f64::from(zoom)));
        }
        let input = iced_interact::input(event)?;
        let hit = iced_interact::hit(bounds, cursor);
        scalar(
            &self.path,
            self.drag
                .on_input(&mut state.drag, input, &hit, Instant::now())
                .map(f64::from),
        )
    }
}

impl MiniWaveCanvas<'_> {
    fn track_position(&self, bounds: Rectangle, cursor: Cursor) -> Option<f32> {
        let position = cursor.position()?;
        let window = window_bounds(self.data.progress, self.data.zoom);
        x_to_norm(position.x - bounds.x, &window, bounds.width)
    }
}

const fn local_bounds(bounds: &Rectangle) -> Rect {
    Rect {
        h: bounds.height,
        w: bounds.width,
        x: 0.0,
        y: 0.0,
    }
}

const fn iced_bounds(bounds: Rect) -> Rectangle {
    Rectangle {
        height: bounds.h,
        width: bounds.w,
        x: bounds.x,
        y: bounds.y,
    }
}
