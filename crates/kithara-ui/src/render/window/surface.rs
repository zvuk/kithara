#[cfg(feature = "iced")]
use iced::Element;

use crate::{
    draw::{DrawList, Pt, Rect},
    interact::CursorShape,
    render::{HostLayer, LayerHit, WindowCommand, WindowEdge, WindowLayerProgram},
    shaping::TextResources,
    solve::{Length, Size},
};

pub(crate) struct WindowSurface {
    height: Length,
    width: Length,
    cursor: CursorShape,
    command: WindowCommand,
}

impl WindowSurface {
    pub(crate) const fn drag() -> Self {
        Self {
            command: WindowCommand::Drag,
            width: Length::Fill,
            height: Length::Fill,
            cursor: CursorShape::None,
        }
    }

    pub(crate) fn frame(bounds: Rect, thickness: f32) -> HostLayer<WindowCommand> {
        let side_width = (bounds.w - thickness * 2.0).max(0.0);
        let side_height = (bounds.h - thickness * 2.0).max(0.0);
        let east = bounds.x + bounds.w - thickness;
        let south = bounds.y + bounds.h - thickness;
        let hit = |area: Rect, edge| {
            let surface = Self::resize(edge, Length::Fixed(area.w), Length::Fixed(area.h));
            LayerHit::new(area, surface.cursor, surface.command)
        };
        HostLayer::new(
            bounds,
            DrawList::default(),
            vec![
                hit(
                    Rect {
                        h: thickness,
                        w: thickness,
                        x: bounds.x,
                        y: bounds.y,
                    },
                    WindowEdge::NorthWest,
                ),
                hit(
                    Rect {
                        h: thickness,
                        w: side_width,
                        x: bounds.x + thickness,
                        y: bounds.y,
                    },
                    WindowEdge::North,
                ),
                hit(
                    Rect {
                        h: thickness,
                        w: thickness,
                        x: east,
                        y: bounds.y,
                    },
                    WindowEdge::NorthEast,
                ),
                hit(
                    Rect {
                        h: side_height,
                        w: thickness,
                        x: bounds.x,
                        y: bounds.y + thickness,
                    },
                    WindowEdge::West,
                ),
                hit(
                    Rect {
                        h: side_height,
                        w: thickness,
                        x: east,
                        y: bounds.y + thickness,
                    },
                    WindowEdge::East,
                ),
                hit(
                    Rect {
                        h: thickness,
                        w: thickness,
                        x: bounds.x,
                        y: south,
                    },
                    WindowEdge::SouthWest,
                ),
                hit(
                    Rect {
                        h: thickness,
                        w: side_width,
                        x: bounds.x + thickness,
                        y: south,
                    },
                    WindowEdge::South,
                ),
                hit(
                    Rect {
                        h: thickness,
                        w: thickness,
                        x: east,
                        y: south,
                    },
                    WindowEdge::SouthEast,
                ),
            ],
        )
    }

    const fn program(&self) -> SurfaceProgram {
        SurfaceProgram {
            command: self.command,
            cursor: self.cursor,
            height: self.height,
            width: self.width,
        }
    }

    pub(crate) const fn resize(edge: WindowEdge, width: Length, height: Length) -> Self {
        Self {
            width,
            height,
            command: WindowCommand::Resize(edge),
            cursor: resize_cursor(edge),
        }
    }
}

#[cfg(feature = "iced")]
impl<'a> crate::render::Widget<'a> for WindowSurface {
    fn view(self) -> Element<'a, crate::render::UiEvent> {
        crate::render::window_layer(self.program())
    }
}

struct SurfaceProgram {
    command: WindowCommand,
    cursor: CursorShape,
    height: Length,
    width: Length,
}

impl WindowLayerProgram for SurfaceProgram {
    type State = ();

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layer(&self, _state: &(), bounds: Rect, _pointer: Option<Pt>) -> HostLayer<WindowCommand> {
        HostLayer::new(
            bounds,
            DrawList::default(),
            vec![LayerHit::new(bounds, self.cursor, self.command)],
        )
    }

    fn resources(&self) -> Option<&TextResources> {
        None
    }
}

const fn resize_cursor(edge: WindowEdge) -> CursorShape {
    match edge {
        WindowEdge::North | WindowEdge::South => CursorShape::ResizeV,
        WindowEdge::East | WindowEdge::West => CursorShape::ResizeH,
        WindowEdge::NorthWest | WindowEdge::SouthEast => CursorShape::ResizeDiagonalDown,
        WindowEdge::NorthEast | WindowEdge::SouthWest => CursorShape::ResizeDiagonalUp,
    }
}

#[cfg(test)]
mod tests {
    use iced::{event, window::RedrawRequest};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        draw::{Pt, Rect},
        interact::{CursorShape, Input, Outcome, PointerPhase, mouse as mouse_input},
        render::{UiEvent, WindowCommand, WindowEdge, window},
    };

    fn pointer_down() -> Input<'static> {
        Input::Pointer(mouse_input(PointerPhase::Down, None))
    }

    /// The retained host's control census calls the window-drag region a
    /// control with no picture rather than one still waiting for a painter.
    /// That is only honest while this host draws nothing for it either, and
    /// while the region still earns its place by carrying the window.
    #[kithara::test]
    fn a_drag_surface_carries_the_window_and_draws_nothing() {
        let bounds = Rect {
            h: 40.0,
            w: 200.0,
            x: 0.0,
            y: 0.0,
        };
        let pointer = Some(Pt { x: 10.0, y: 10.0 });
        let layer = WindowSurface::drag().program().layer(&(), bounds, pointer);

        assert!(layer.draw().commands().is_empty());
        assert_eq!(layer.action_at(pointer), Some(&WindowCommand::Drag));
    }

    #[kithara::test]
    fn every_resize_edge_has_its_exact_region_command_and_cursor() {
        let layer = WindowSurface::frame(
            Rect {
                h: 60.0,
                w: 100.0,
                x: 0.0,
                y: 0.0,
            },
            4.0,
        );
        let expected = [
            (
                Rect {
                    h: 4.0,
                    w: 4.0,
                    x: 0.0,
                    y: 0.0,
                },
                WindowEdge::NorthWest,
                CursorShape::ResizeDiagonalDown,
            ),
            (
                Rect {
                    h: 4.0,
                    w: 92.0,
                    x: 4.0,
                    y: 0.0,
                },
                WindowEdge::North,
                CursorShape::ResizeV,
            ),
            (
                Rect {
                    h: 4.0,
                    w: 4.0,
                    x: 96.0,
                    y: 0.0,
                },
                WindowEdge::NorthEast,
                CursorShape::ResizeDiagonalUp,
            ),
            (
                Rect {
                    h: 52.0,
                    w: 4.0,
                    x: 0.0,
                    y: 4.0,
                },
                WindowEdge::West,
                CursorShape::ResizeH,
            ),
            (
                Rect {
                    h: 52.0,
                    w: 4.0,
                    x: 96.0,
                    y: 4.0,
                },
                WindowEdge::East,
                CursorShape::ResizeH,
            ),
            (
                Rect {
                    h: 4.0,
                    w: 4.0,
                    x: 0.0,
                    y: 56.0,
                },
                WindowEdge::SouthWest,
                CursorShape::ResizeDiagonalUp,
            ),
            (
                Rect {
                    h: 4.0,
                    w: 92.0,
                    x: 4.0,
                    y: 56.0,
                },
                WindowEdge::South,
                CursorShape::ResizeV,
            ),
            (
                Rect {
                    h: 4.0,
                    w: 4.0,
                    x: 96.0,
                    y: 56.0,
                },
                WindowEdge::SouthEast,
                CursorShape::ResizeDiagonalDown,
            ),
        ];

        assert_eq!(layer.hits().len(), expected.len());
        for (hit, (area, edge, cursor)) in layer.hits().iter().zip(expected) {
            assert_eq!(hit.area(), area, "wrong area for {edge:?}");
            assert_eq!(hit.action(), &WindowCommand::Resize(edge));
            assert_eq!(hit.cursor(), cursor, "wrong cursor for {edge:?}");
            let pointer = Pt {
                x: area.x + area.w / 2.0,
                y: area.y + area.h / 2.0,
            };
            let outcome = layer.handle(pointer_down(), Some(pointer));
            assert_eq!(
                outcome,
                Outcome::set(WindowCommand::Resize(edge)),
                "wrong command emitted for {edge:?}"
            );
            let action = window(WindowCommand::Resize(edge), outcome.map(|_| ()))
                .unwrap_or_else(|| panic!("{edge:?} must bind to a window event"));
            assert_eq!(
                action.into_inner(),
                (
                    Some(UiEvent::Window(WindowCommand::Resize(edge))),
                    RedrawRequest::Wait,
                    event::Status::Captured,
                ),
                "wrong event bound for {edge:?}"
            );
            assert_eq!(
                layer.cursor_at(Some(pointer)),
                cursor,
                "wrong cursor reported for {edge:?}"
            );
        }
    }

    #[kithara::test]
    fn the_drag_surface_emits_the_window_drag_command() {
        let surface = WindowSurface::drag();
        let program = SurfaceProgram {
            command: surface.command,
            cursor: surface.cursor,
            height: surface.height,
            width: surface.width,
        };
        let bounds = Rect {
            h: 32.0,
            w: 180.0,
            x: 17.0,
            y: 9.0,
        };
        let pointer = Some(Pt { x: 90.0, y: 16.0 });
        let layer = program.layer(&(), bounds, None);
        let (outcome, redraw) = program.update(&mut (), pointer_down(), &layer, pointer);

        assert_eq!(outcome, Outcome::set(WindowCommand::Drag));
        assert!(!redraw);
        assert_eq!(layer.bounds(), bounds);
        assert_eq!(layer.hits()[0].area(), bounds);
        assert_eq!(layer.cursor_at(pointer), CursorShape::None);
    }
}
