use crate::{
    draw::{DrawList, DrawListBuilder, Pt, Rect, Rgba},
    hosts::layer::{HostLayer, LayerHit, WindowLayerProgram},
    interact::{CursorShape, Hit, Input, Outcome, PointerPhase},
    module::WindowControlsStyle,
    render::{Skin, WindowCommand},
    skin::{FrameSkin, WindowControlSkin},
};

#[derive(Clone, Copy)]
enum Glyph {
    Minus,
    Square,
    Close,
}

#[derive(Clone, Copy)]
pub(crate) struct ControlRegion {
    glyph: Glyph,
    pub(crate) bounds: Rect,
    pub(crate) command: WindowCommand,
    icon_size: f32,
}

pub(crate) struct ControlsProgram {
    divider_color: Option<Rgba>,
    frame_color: Option<Rgba>,
    color: Rgba,
    hover_color: Rgba,
    pub(crate) controls: WindowControlSkin,
    stroke_width: f32,
}

#[derive(Default)]
pub(crate) struct ControlsState {
    pub(crate) armed: Option<WindowCommand>,
    pub(crate) hovered: Option<WindowCommand>,
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
            controls,
            divider_color,
            frame_color,
            color: skin.rgba(skin.window.icon_color),
            hover_color: skin.rgba(skin.window.icon_hover_color),
            stroke_width: skin.window.icon_stroke_width,
        }
    }

    pub(crate) fn hits(&self, bounds: Rect) -> Vec<LayerHit<WindowCommand>> {
        let (regions, count) = self.regions(bounds);
        regions
            .into_iter()
            .take(count)
            .map(|region| LayerHit::new(region.bounds, CursorShape::Pointer, region.command))
            .collect()
    }

    pub(crate) fn paint(&self, bounds: Rect, at: Option<Pt>) -> DrawList {
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

    pub(crate) fn regions(&self, bounds: Rect) -> ([ControlRegion; 3], usize) {
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
                    icon_size,
                    bounds: Rect {
                        h: bounds.h,
                        w: cell_size,
                        x: bounds.x,
                        y: bounds.y,
                    },
                    command: WindowCommand::Close,
                    glyph: Glyph::Close,
                };
                ([close; 3], 1)
            }
        }
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

    fn hit_layer(&self, _state: &ControlsState, bounds: Rect) -> HostLayer<WindowCommand> {
        HostLayer::new(bounds, DrawList::default(), self.hits(bounds))
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

    fn update(
        &self,
        state: &mut ControlsState,
        input: Input<'_>,
        layer: &HostLayer<WindowCommand>,
        pointer: Option<Pt>,
    ) -> (Outcome<WindowCommand>, bool) {
        let target = layer.action_at(pointer).copied();
        let outcome = match input {
            Input::Pointer(pointer) if pointer.presses_primary() => {
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
}
