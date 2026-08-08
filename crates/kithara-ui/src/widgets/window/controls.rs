use iced::{Element, Length, Size};

use crate::{
    draw::{DrawList, DrawListBuilder, Pt, Rect, Rgba},
    interact::{CursorShape, Hit, Input, Outcome, PointerPhase},
    module::WindowControlsStyle,
    render::{HostLayer, LayerHit, Skin, UiEvent, WindowCommand, WindowLayerProgram, window_layer},
    skin::{FrameSkin, WindowControlSkin},
    text::TextResources,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct WindowControls<'skin> {
    skin: &'skin Skin,
    style: WindowControlsStyle,
}

impl<'a> Widget<'a> for WindowControls<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        window_layer(ControlsProgram::new(self.style, self.skin))
    }
}

#[derive(Clone, Copy)]
enum Glyph {
    Minus,
    Square,
    Close,
}

#[derive(Clone, Copy)]
struct ControlRegion {
    bounds: Rect,
    command: WindowCommand,
    glyph: Glyph,
    icon_size: f32,
}

pub(crate) struct ControlsProgram {
    color: Rgba,
    controls: WindowControlSkin,
    divider_color: Option<Rgba>,
    frame_color: Option<Rgba>,
    hover_color: Rgba,
    resources: TextResources,
    stroke_width: f32,
}

#[derive(Default)]
pub(crate) struct ControlsState {
    armed: Option<WindowCommand>,
    hovered: Option<WindowCommand>,
}

impl ControlsProgram {
    pub(crate) fn new(style: WindowControlsStyle, skin: &Skin) -> Self {
        let controls = skin.window.controls(style);
        let divider_color = match controls {
            WindowControlSkin::Close {
                divider: Some((_, role)),
                ..
            } => Some(skin.rgba(role)),
            WindowControlSkin::Buttons { .. } | WindowControlSkin::Close { .. } => None,
        };
        let frame_color = match controls {
            WindowControlSkin::Close {
                frame: Some(frame), ..
            } => Some(skin.rgba(frame.border)),
            WindowControlSkin::Buttons { .. } | WindowControlSkin::Close { .. } => None,
        };
        Self {
            color: skin.rgba(skin.window.icon_color),
            controls,
            divider_color,
            frame_color,
            hover_color: skin.rgba(skin.window.icon_hover_color),
            resources: skin.text_resources().clone(),
            stroke_width: skin.window.icon_stroke_width,
        }
    }

    fn width(&self) -> f32 {
        match self.controls {
            WindowControlSkin::Buttons {
                minus_icon_size,
                maximize_icon_size,
                close_icon_size,
                gap,
                padding,
            } => minus_icon_size + maximize_icon_size + close_icon_size + gap * 2.0 + padding * 2.0,
            WindowControlSkin::Close { cell_size, .. } => cell_size,
        }
    }

    fn height(&self) -> Length {
        match self.controls {
            WindowControlSkin::Close {
                cell_size,
                divider: Some(_),
                ..
            } => Length::Fixed(cell_size),
            WindowControlSkin::Buttons { .. } | WindowControlSkin::Close { .. } => Length::Fill,
        }
    }

    fn regions(&self, bounds: Rect) -> ([ControlRegion; 3], usize) {
        match self.controls {
            WindowControlSkin::Buttons {
                minus_icon_size,
                maximize_icon_size,
                close_icon_size,
                gap,
                padding,
            } => {
                let minus = Rect {
                    h: bounds.h,
                    w: minus_icon_size,
                    x: bounds.x + padding,
                    y: bounds.y,
                };
                let maximize = Rect {
                    h: bounds.h,
                    w: maximize_icon_size,
                    x: minus.x + minus.w + gap,
                    y: bounds.y,
                };
                let close = Rect {
                    h: bounds.h,
                    w: close_icon_size,
                    x: maximize.x + maximize.w + gap,
                    y: bounds.y,
                };
                (
                    [
                        ControlRegion {
                            bounds: minus,
                            command: WindowCommand::Minimize,
                            glyph: Glyph::Minus,
                            icon_size: minus_icon_size,
                        },
                        ControlRegion {
                            bounds: maximize,
                            command: WindowCommand::ToggleMaximize,
                            glyph: Glyph::Square,
                            icon_size: maximize_icon_size,
                        },
                        ControlRegion {
                            bounds: close,
                            command: WindowCommand::Close,
                            glyph: Glyph::Close,
                            icon_size: close_icon_size,
                        },
                    ],
                    3,
                )
            }
            WindowControlSkin::Close {
                cell_size,
                icon_size,
                ..
            } => {
                let close = ControlRegion {
                    bounds: Rect {
                        h: bounds.h,
                        w: cell_size,
                        x: bounds.x,
                        y: bounds.y,
                    },
                    command: WindowCommand::Close,
                    glyph: Glyph::Close,
                    icon_size,
                };
                ([close; 3], 1)
            }
        }
    }

    fn hits(&self, bounds: Rect) -> Vec<LayerHit<WindowCommand>> {
        let (regions, count) = self.regions(bounds);
        regions
            .into_iter()
            .take(count)
            .map(|region| LayerHit::new(region.bounds, CursorShape::Pointer, region.command))
            .collect()
    }

    fn paint(&self, bounds: Rect, at: Option<Pt>) -> DrawList {
        let mut builder = DrawListBuilder::default();
        if let (
            WindowControlSkin::Close {
                frame: Some(frame), ..
            },
            Some(color),
        ) = (self.controls, self.frame_color)
        {
            paint_frame(&mut builder, bounds, frame, color);
        }
        let (regions, count) = self.regions(bounds);
        for region in regions.into_iter().take(count) {
            let color = if Hit::new(at, region.bounds).over() {
                self.hover_color
            } else {
                self.color
            };
            paint_glyph(
                &mut builder,
                region.glyph,
                region.bounds,
                region.icon_size,
                color,
                self.stroke_width,
            );
        }
        if let (
            WindowControlSkin::Close {
                divider: Some((width, _)),
                ..
            },
            Some(color),
        ) = (self.controls, self.divider_color)
        {
            builder.fill_rect(
                Rect {
                    h: bounds.h,
                    w: width.min(bounds.w),
                    ..bounds
                },
                color,
            );
        }
        builder.finish()
    }
}

fn paint_frame(builder: &mut DrawListBuilder, bounds: Rect, frame: FrameSkin, color: Rgba) {
    if frame.border_width <= 0.0 {
        return;
    }
    let inset = frame.border_width / 2.0;
    builder.stroke_rounded_rect(
        Rect {
            h: (bounds.h - frame.border_width).max(0.0),
            w: (bounds.w - frame.border_width).max(0.0),
            x: bounds.x + inset,
            y: bounds.y + inset,
        },
        frame.radius,
        color,
        frame.border_width,
    );
}

fn paint_glyph(
    builder: &mut DrawListBuilder,
    glyph: Glyph,
    bounds: Rect,
    size: f32,
    color: Rgba,
    width: f32,
) {
    let center = Pt {
        x: bounds.x + bounds.w / 2.0,
        y: bounds.y + bounds.h / 2.0,
    };
    let half = size / 2.0;
    match glyph {
        Glyph::Minus => builder.stroke_line(
            Pt {
                x: center.x - half,
                y: center.y,
            },
            Pt {
                x: center.x + half,
                y: center.y,
            },
            color,
            width,
        ),
        Glyph::Square => builder.stroke_rounded_rect(
            Rect {
                h: size,
                w: size,
                x: center.x - half,
                y: center.y - half,
            },
            0.0,
            color,
            width,
        ),
        Glyph::Close => {
            builder.stroke_line(
                Pt {
                    x: center.x - half,
                    y: center.y - half,
                },
                Pt {
                    x: center.x + half,
                    y: center.y + half,
                },
                color,
                width,
            );
            builder.stroke_line(
                Pt {
                    x: center.x + half,
                    y: center.y - half,
                },
                Pt {
                    x: center.x - half,
                    y: center.y + half,
                },
                color,
                width,
            );
        }
    }
}

impl WindowLayerProgram for ControlsProgram {
    type State = ControlsState;

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width()), self.height())
    }

    fn layer(
        &self,
        _state: &ControlsState,
        bounds: Rect,
        pointer: Option<Pt>,
    ) -> HostLayer<WindowCommand> {
        let local_pointer = pointer.map(|point| Pt {
            x: point.x - bounds.x,
            y: point.y - bounds.y,
        });
        let local_bounds = Rect {
            x: 0.0,
            y: 0.0,
            ..bounds
        };
        HostLayer::new(
            bounds,
            self.paint(local_bounds, local_pointer),
            self.hits(bounds),
        )
    }

    fn hit_layer(&self, _state: &ControlsState, bounds: Rect) -> HostLayer<WindowCommand> {
        HostLayer::new(bounds, DrawList::default(), self.hits(bounds))
    }

    fn update(
        &self,
        state: &mut ControlsState,
        input: Input<'_>,
        layer: &HostLayer<WindowCommand>,
        pointer: Option<Pt>,
    ) -> (Outcome<WindowCommand>, bool) {
        let target = layer.action_at(pointer).copied();
        let outcome = match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down => {
                state.armed = target;
                if target.is_some() {
                    Outcome::captured().with_ownership(crate::interact::PointerOwnership::Claim)
                } else {
                    Outcome::IGNORED
                }
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up => {
                match state.armed.take() {
                    Some(command) if target == Some(command) => Outcome::set(command)
                        .with_ownership(crate::interact::PointerOwnership::Release),
                    Some(_) => Outcome::captured()
                        .with_ownership(crate::interact::PointerOwnership::Release),
                    None => Outcome::IGNORED,
                }
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Cancel => {
                let armed = state.armed.take().is_some();
                if armed {
                    Outcome::captured().with_ownership(crate::interact::PointerOwnership::Release)
                } else {
                    Outcome::IGNORED
                }
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        };
        let hovered = match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Move => target,
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Leave => None,
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => state.hovered,
        };
        let redraw = state.hovered != hovered;
        state.hovered = hovered;
        (outcome, redraw)
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
        draw::{DrawCmd, Geom, Paint},
        interact::mouse as mouse_input,
    };

    fn pointer_input(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
        Input::Pointer(mouse_input(phase, at))
    }

    #[kithara::test]
    fn styles_select_their_skin_metrics() {
        let window = builtin::skin_doc().window;

        assert!(matches!(
            window.controls(WindowControlsStyle::Standard),
            WindowControlSkin::Buttons {
                minus_icon_size: 11.0,
                maximize_icon_size: 10.0,
                close_icon_size: 11.0,
                gap: 12.0,
                padding: 12.0,
            }
        ));
        assert!(matches!(
            window.controls(WindowControlsStyle::Compact),
            WindowControlSkin::Buttons {
                minus_icon_size: 10.0,
                maximize_icon_size: 9.0,
                close_icon_size: 10.0,
                gap: 10.0,
                padding: 10.0,
            }
        ));
        assert!(matches!(
            window.controls(WindowControlsStyle::CloseWide),
            WindowControlSkin::Close {
                cell_size: 32.0,
                icon_size: 11.0,
                divider: Some((1.0, _)),
                ..
            }
        ));
        assert!(matches!(
            window.controls(WindowControlsStyle::CloseMicro),
            WindowControlSkin::Close {
                cell_size: 28.0,
                icon_size: 10.0,
                frame: None,
                divider: None,
            }
        ));
        assert!(matches!(
            window.controls(WindowControlsStyle::CloseFramed),
            WindowControlSkin::Close {
                cell_size: 22.0,
                icon_size: 10.0,
                frame: Some(_),
                divider: None,
            }
        ));
    }

    fn region_table(style: WindowControlsStyle) -> Vec<(WindowCommand, Rect)> {
        let program = ControlsProgram::new(style, builtin::skin());
        let (regions, count) = program.regions(Rect {
            h: 32.0,
            w: program.width(),
            x: 0.0,
            y: 0.0,
        });
        regions[..count]
            .iter()
            .map(|region| (region.command, region.bounds))
            .collect()
    }

    #[kithara::test]
    fn every_style_keeps_its_exact_interactive_regions() {
        assert_eq!(
            region_table(WindowControlsStyle::Standard),
            [
                (
                    WindowCommand::Minimize,
                    Rect {
                        h: 32.0,
                        w: 11.0,
                        x: 12.0,
                        y: 0.0,
                    },
                ),
                (
                    WindowCommand::ToggleMaximize,
                    Rect {
                        h: 32.0,
                        w: 10.0,
                        x: 35.0,
                        y: 0.0,
                    },
                ),
                (
                    WindowCommand::Close,
                    Rect {
                        h: 32.0,
                        w: 11.0,
                        x: 57.0,
                        y: 0.0,
                    },
                ),
            ]
        );
        assert_eq!(
            region_table(WindowControlsStyle::Compact),
            [
                (
                    WindowCommand::Minimize,
                    Rect {
                        h: 32.0,
                        w: 10.0,
                        x: 10.0,
                        y: 0.0,
                    },
                ),
                (
                    WindowCommand::ToggleMaximize,
                    Rect {
                        h: 32.0,
                        w: 9.0,
                        x: 30.0,
                        y: 0.0,
                    },
                ),
                (
                    WindowCommand::Close,
                    Rect {
                        h: 32.0,
                        w: 10.0,
                        x: 49.0,
                        y: 0.0,
                    },
                ),
            ]
        );
        for (style, size) in [
            (WindowControlsStyle::CloseWide, 32.0),
            (WindowControlsStyle::CloseMicro, 28.0),
            (WindowControlsStyle::CloseFramed, 22.0),
        ] {
            assert_eq!(
                region_table(style),
                [(
                    WindowCommand::Close,
                    Rect {
                        h: 32.0,
                        w: size,
                        x: 0.0,
                        y: 0.0,
                    },
                )],
                "{style:?}"
            );
        }
    }

    #[kithara::test]
    fn every_style_keeps_its_existing_layout_lengths() {
        for (style, width, height) in [
            (WindowControlsStyle::Standard, 80.0, Length::Fill),
            (WindowControlsStyle::Compact, 69.0, Length::Fill),
            (WindowControlsStyle::CloseWide, 32.0, Length::Fixed(32.0)),
            (WindowControlsStyle::CloseMicro, 28.0, Length::Fill),
            (WindowControlsStyle::CloseFramed, 22.0, Length::Fill),
        ] {
            let program = ControlsProgram::new(style, builtin::skin());
            assert_eq!(program.width(), width, "{style:?} width");
            assert_eq!(program.height(), height, "{style:?} height");
            assert_eq!(
                program.size(),
                Size::new(Length::Fixed(width), height),
                "{style:?} layer size"
            );
        }
    }

    #[kithara::test]
    fn hover_changes_only_the_glyph_under_the_pointer() {
        let skin = builtin::skin();
        let program = ControlsProgram::new(WindowControlsStyle::Standard, skin);
        let list = program.paint(
            Rect {
                h: 32.0,
                w: program.width(),
                x: 0.0,
                y: 0.0,
            },
            Some(Pt { x: 17.5, y: 16.0 }),
        );
        let colors = list
            .commands()
            .iter()
            .filter_map(|command| match command {
                DrawCmd::Stroke { color, .. } => Some(*color),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(colors.len(), 4);
        assert_eq!(colors[0], skin.rgba(skin.window.icon_hover_color));
        assert!(
            colors[1..]
                .iter()
                .all(|color| *color == skin.rgba(skin.window.icon_color))
        );
    }

    #[kithara::test]
    fn close_styles_retain_their_frame_and_divider_commands() {
        let skin = builtin::skin();
        let framed = ControlsProgram::new(WindowControlsStyle::CloseFramed, skin);
        let framed_list = framed.paint(
            Rect {
                h: 22.0,
                w: framed.width(),
                x: 0.0,
                y: 0.0,
            },
            None,
        );
        let WindowControlSkin::Close {
            frame: Some(frame), ..
        } = skin.window.controls(WindowControlsStyle::CloseFramed)
        else {
            panic!("the close-framed style must carry a frame");
        };
        assert!(matches!(
            framed_list.commands().first(),
            Some(DrawCmd::Stroke {
                geom: Geom::Rect(_),
                color,
                pen,
            }) if *color == skin.rgba(frame.border) && pen.width == frame.border_width
        ));

        let wide = ControlsProgram::new(WindowControlsStyle::CloseWide, skin);
        let wide_list = wide.paint(
            Rect {
                h: 32.0,
                w: wide.width(),
                x: 0.0,
                y: 0.0,
            },
            None,
        );
        let WindowControlSkin::Close {
            divider: Some((divider_width, divider_role)),
            ..
        } = skin.window.controls(WindowControlsStyle::CloseWide)
        else {
            panic!("the close-wide style must carry a divider");
        };
        assert!(matches!(
            wide_list.commands().last(),
            Some(DrawCmd::Fill {
                geom: Geom::Rect(Rect { h: 32.0, w, x: 0.0, y: 0.0 }),
                paint: Paint::Solid(color),
            }) if *w == divider_width && *color == skin.rgba(divider_role)
        ));
    }

    #[kithara::test]
    fn layer_keeps_paint_local_and_hits_absolute() {
        let program = ControlsProgram::new(WindowControlsStyle::Standard, builtin::skin());
        let bounds = Rect {
            h: 32.0,
            w: program.width(),
            x: 100.0,
            y: 40.0,
        };
        let state = ControlsState::default();
        let pointer = Pt { x: 117.5, y: 56.0 };
        let layer = program.layer(&state, bounds, Some(pointer));

        assert_eq!(
            layer.draw(),
            &program.paint(
                Rect {
                    h: 32.0,
                    w: program.width(),
                    x: 0.0,
                    y: 0.0,
                },
                Some(Pt { x: 17.5, y: 16.0 }),
            )
        );
        assert_eq!(
            layer.hits()[0].area(),
            Rect {
                h: 32.0,
                w: 11.0,
                x: 112.0,
                y: 40.0,
            }
        );
        assert_eq!(layer.cursor_at(Some(pointer)), CursorShape::Pointer);
        assert_eq!(
            layer.cursor_at(Some(Pt { x: 129.0, y: 56.0 })),
            CursorShape::None,
            "the gap between buttons must not claim the pointer",
        );

        let hit_layer = program.hit_layer(&state, bounds);
        assert!(hit_layer.draw().commands().is_empty());
        assert_eq!(hit_layer.hits(), layer.hits());
    }

    #[kithara::test]
    fn a_gap_does_not_arm_or_capture() {
        let program = ControlsProgram::new(WindowControlsStyle::Standard, builtin::skin());
        let bounds = control_bounds(&program);
        let mut state = ControlsState::default();
        let pointer = absolute(bounds, Pt { x: 29.0, y: 16.0 });
        let layer = program.hit_layer(&state, bounds);
        let (outcome, redraw) = program.update(
            &mut state,
            pointer_input(PointerPhase::Down, None),
            &layer,
            Some(pointer),
        );

        assert_eq!(outcome, Outcome::IGNORED);
        assert!(!redraw);
        assert_eq!(state.armed, None);
    }

    fn control_bounds(program: &ControlsProgram) -> Rect {
        Rect {
            h: 32.0,
            w: program.width(),
            x: 100.0,
            y: 40.0,
        }
    }

    fn absolute(bounds: Rect, local: Pt) -> Pt {
        Pt {
            x: bounds.x + local.x,
            y: bounds.y + local.y,
        }
    }

    fn assert_release(program: &ControlsProgram, local: Pt, command: WindowCommand) {
        let bounds = control_bounds(program);
        let pointer = absolute(bounds, local);
        let mut state = ControlsState::default();
        let layer = program.hit_layer(&state, bounds);
        let (pressed, redraw) = program.update(
            &mut state,
            pointer_input(PointerPhase::Down, None),
            &layer,
            Some(pointer),
        );
        assert_eq!(
            pressed,
            Outcome::captured().with_ownership(crate::interact::PointerOwnership::Claim)
        );
        assert!(!redraw);
        assert_eq!(state.armed, Some(command));

        let (released, redraw) = program.update(
            &mut state,
            pointer_input(PointerPhase::Up, None),
            &layer,
            Some(pointer),
        );

        assert_eq!(
            released,
            Outcome::set(command).with_ownership(crate::interact::PointerOwnership::Release)
        );
        assert!(!redraw);
        assert_eq!(state.armed, None);
    }

    #[kithara::test]
    fn standard_buttons_and_close_only_controls_emit_their_own_commands() {
        let standard = ControlsProgram::new(WindowControlsStyle::Standard, builtin::skin());
        assert_release(&standard, Pt { x: 17.5, y: 16.0 }, WindowCommand::Minimize);
        assert_release(
            &standard,
            Pt { x: 40.0, y: 16.0 },
            WindowCommand::ToggleMaximize,
        );
        assert_release(&standard, Pt { x: 62.5, y: 16.0 }, WindowCommand::Close);

        let close = ControlsProgram::new(WindowControlsStyle::CloseFramed, builtin::skin());
        assert_release(&close, Pt { x: 11.0, y: 16.0 }, WindowCommand::Close);
    }

    #[kithara::test]
    fn leaving_a_window_button_before_release_cancels_its_command() {
        let program = ControlsProgram::new(WindowControlsStyle::Standard, builtin::skin());
        let bounds = control_bounds(&program);
        let mut state = ControlsState::default();
        let layer = program.hit_layer(&state, bounds);
        let pointer = absolute(bounds, Pt { x: 17.5, y: 16.0 });
        let (pressed, _) = program.update(
            &mut state,
            pointer_input(PointerPhase::Down, None),
            &layer,
            Some(pointer),
        );
        assert_eq!(
            pressed,
            Outcome::captured().with_ownership(crate::interact::PointerOwnership::Claim)
        );

        let outside = absolute(bounds, Pt { x: 90.0, y: 16.0 });
        let (released, redraw) = program.update(
            &mut state,
            pointer_input(PointerPhase::Up, None),
            &layer,
            Some(outside),
        );

        assert_eq!(
            released,
            Outcome::captured().with_ownership(crate::interact::PointerOwnership::Release)
        );
        assert!(!redraw);
        assert_eq!(state.armed, None);
    }

    #[kithara::test]
    fn hover_transitions_request_only_the_needed_repaints() {
        let program = ControlsProgram::new(WindowControlsStyle::Standard, builtin::skin());
        let bounds = control_bounds(&program);
        let mut state = ControlsState::default();
        let layer = program.hit_layer(&state, bounds);
        let minimize = absolute(bounds, Pt { x: 17.5, y: 16.0 });

        let (outcome, redraw) = program.update(
            &mut state,
            pointer_input(PointerPhase::Move, Some(minimize)),
            &layer,
            Some(minimize),
        );
        assert_eq!(outcome, Outcome::IGNORED);
        assert!(redraw);
        assert_eq!(state.hovered, Some(WindowCommand::Minimize));

        let (_, redraw) = program.update(
            &mut state,
            pointer_input(PointerPhase::Move, Some(minimize)),
            &layer,
            Some(minimize),
        );
        assert!(!redraw);

        let (_, redraw) = program.update(
            &mut state,
            pointer_input(PointerPhase::Leave, None),
            &layer,
            None,
        );
        assert!(redraw);
        assert_eq!(state.hovered, None);
    }
}
