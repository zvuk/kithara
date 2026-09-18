use std::cell::RefCell;

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme,
    keyboard::{Event as KeyboardEvent, Modifiers},
    mouse::{self, Button, Cursor, Interaction},
    widget::canvas::{self, Action, Canvas, Geometry},
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
    render::{Skin, UiEvent, Widget, controls::snapped, immediate::cache, scalar, scalar_child},
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
            drag,
            painter,
            data: self.data,
            path: self.path.to_owned(),
            skin: self.skin,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct MiniWaveCanvas<'skin> {
    skin: &'skin Skin,
    data: Drawn,
    painter: Face,
    drag: Scalar,
    path: String,
}

struct MiniWaveState {
    modifiers: Modifiers,
    loop_start: Option<f32>,
    text: RefCell<Option<TextContext>>,
    drag: ScalarState,
    /// Tessellated picture kept between frames. Without it every frame hands
    /// the renderer freshly built geometry, and its buffer pool grows to the
    /// high-water mark of all of them and never gives it back.
    cache: canvas::Cache,
    /// Everything the picture is built from. The cache is dropped exactly when
    /// this changes, so a reused picture can never be a stale one.
    painted: RefCell<Option<Painted>>,
}

impl Default for MiniWaveState {
    fn default() -> Self {
        Self {
            cache: cache::canvas(),
            drag: ScalarState::default(),
            loop_start: None,
            modifiers: Modifiers::default(),
            painted: RefCell::default(),
            text: RefCell::default(),
        }
    }
}

/// The inputs that decide what the wave looks like.
#[derive(PartialEq)]
struct Painted {
    data: Drawn,
    bounds: Rectangle,
    overlay: bool,
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
        let placed = bounds;
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
        let bounds = local_bounds(&bounds, placed);
        let stale = state.painted.borrow().as_ref().is_none_or(|painted| {
            painted.data != self.data || painted.bounds != placed || painted.overlay != show_overlay
        });
        if stale {
            state.cache.clear();
            *state.painted.borrow_mut() = Some(Painted {
                bounds: placed,
                data: self.data.clone(),
                overlay: show_overlay,
            });
        }
        let geometry =
            state
                .cache
                .draw(renderer, Size::new(placed.width, placed.height), |frame| {
                    let mut text = state.text.borrow_mut();
                    let text = text.get_or_insert_with(|| self.skin.text_resources().into());
                    let mut list = DrawListBuilder::default();
                    self.painter
                        .paint(&mut list, text, &self.data, bounds, show_overlay);
                    replay_ordered(&list.finish(), frame, self.skin.text_resources());
                });
        vec![geometry]
    }

    fn mouse_interaction(
        &self,
        state: &MiniWaveState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Interaction {
        if self.data.has_waveform() {
            self.drag
                .cursor(&state.drag, &iced_interact::hit(bounds, cursor))
                .into()
        } else {
            Interaction::default()
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
                Event::Mouse(mouse::Event::ButtonPressed(Button::Left))
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
                Event::Mouse(mouse::Event::ButtonReleased(Button::Left))
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

/// The whole-pixel box the painter draws into, in the coordinates of the canvas
/// it is placed at.
///
/// iced lays the geometry down at the box the solver arrived at, fractions and
/// all, so a painter drawing from the canvas origin would put its bars half a
/// pixel off the grid the retained host - which is handed the snapped box -
/// draws them on. Carrying the snap across as an offset lands the same ink on
/// the same pixels in both hosts.
const fn local_bounds(bounds: &Rectangle, placed: Rectangle) -> Rect {
    Rect {
        h: bounds.height,
        w: bounds.width,
        x: bounds.x - placed.x,
        y: bounds.y - placed.y,
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    /// A box the solver put on a half pixel, which is where the two hosts drew
    /// the same waveform on different rows.
    const PLACED: Rectangle = Rectangle {
        height: 120.5,
        width: 262.5,
        x: 197.5,
        y: 182.25,
    };

    /// Where the painter's own origin ends up on the screen.
    fn origin(placed: Rectangle) -> (f32, f32) {
        let local = local_bounds(&snapped(placed), placed);
        (placed.x + local.x, placed.y + local.y)
    }

    #[kithara::test]
    fn a_canvas_placed_off_the_grid_draws_from_the_column_beside_it() {
        assert_eq!(
            origin(PLACED).0,
            198.0,
            "the painter must start on the pixel column the retained host draws from"
        );
    }

    #[kithara::test]
    fn a_canvas_placed_off_the_grid_draws_from_the_row_beside_it() {
        assert_eq!(
            origin(PLACED).1,
            182.0,
            "the painter must start on the pixel row the retained host draws from"
        );
    }

    #[kithara::test]
    fn a_canvas_already_on_the_grid_draws_from_its_own_origin() {
        let placed = Rectangle {
            height: 120.0,
            width: 262.0,
            x: 212.0,
            y: 85.0,
        };
        let local = local_bounds(&snapped(placed), placed);

        assert_eq!((local.x, local.y), (0.0, 0.0));
    }

    #[kithara::test]
    fn the_painter_is_handed_the_size_the_snapped_box_covers() {
        let local = local_bounds(&snapped(PLACED), PLACED);

        assert_eq!((local.w, local.h), (262.0, 121.0));
    }
}
