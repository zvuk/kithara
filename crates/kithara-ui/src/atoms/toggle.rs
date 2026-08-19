use crate::{
    atoms::design::quad::border,
    draw::{DrawListBuilder, Rect, Rgba},
    render::Skin,
    skin::FrameSkin,
};

/// A switch with two states and nothing else: the toggle and the checkbox are
/// the same picture, one of them with a thumb sliding across it.
pub(crate) struct Binary {
    active: Face,
    idle: Face,
    thumb: Option<Thumb>,
}

/// How the switch looks in one of its states. Both are resolved from the skin
/// when the switch is built, so flipping it is a repaint rather than a reason
/// to rebuild the control.
struct Face {
    border: Rgba,
    /// The idle switch has no body of its own: it is a frame around nothing.
    fill: Option<Rgba>,
    frame: FrameSkin,
    thumb: Rgba,
}

#[derive(Clone, Copy)]
struct Thumb {
    inset: f32,
    radius: f32,
    size: f32,
}

impl Binary {
    pub(crate) fn toggle(skin: &Skin) -> Self {
        let metrics = skin.toggle;
        Self {
            active: Face {
                border: skin.rgba(metrics.active_frame.border),
                fill: Some(skin.palette.accent),
                frame: metrics.active_frame,
                thumb: skin.palette.bg_deep,
            },
            idle: Face {
                border: skin.rgba(metrics.inactive_frame.border),
                fill: None,
                frame: metrics.inactive_frame,
                thumb: skin.palette.muted,
            },
            thumb: Some(Thumb {
                inset: metrics.thumb_inset,
                radius: metrics.thumb_radius,
                size: metrics.thumb_size,
            }),
        }
    }

    pub(crate) fn checkbox(skin: &Skin) -> Self {
        let metrics = skin.checkbox;
        Self {
            active: Face {
                border: skin.rgba(metrics.active_frame.border),
                fill: Some(skin.palette.accent),
                frame: metrics.active_frame,
                thumb: skin.palette.bg_deep,
            },
            idle: Face {
                border: skin.rgba(metrics.inactive_frame.border),
                fill: None,
                frame: metrics.inactive_frame,
                thumb: skin.palette.muted,
            },
            thumb: None,
        }
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, active: bool, bounds: Rect) {
        let face = if active { &self.active } else { &self.idle };
        if let Some(fill) = face.fill {
            list.fill_rounded_rect(bounds, face.frame.radius, fill);
        }
        border(list, bounds, face.frame, face.border);
        let Some(thumb) = self.thumb else {
            return;
        };
        let offset = if active {
            bounds.w - thumb.inset - thumb.size
        } else {
            thumb.inset
        };
        list.fill_rounded_rect(
            Rect {
                h: thumb.size,
                w: thumb.size,
                x: bounds.x + offset,
                y: bounds.y + (bounds.h - thumb.size) / 2.0,
            },
            thumb.radius,
            face.thumb,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Binary, DrawListBuilder, Rect};
    use crate::{
        builtin,
        draw::{DrawCmd, Geom, Paint, Pen},
    };

    const TOGGLE: Rect = Rect {
        h: 14.0,
        w: 26.0,
        x: 0.0,
        y: 0.0,
    };

    /// The thumb travels to the far end when the switch is on, and the body
    /// only exists there — an idle toggle is an outline.
    #[kithara::test]
    fn a_toggle_lights_its_body_and_slides_its_thumb_across() {
        let skin = builtin::skin();
        let toggle = Binary::toggle(skin);
        let draw = |active| {
            let mut list = DrawListBuilder::default();
            toggle.paint(&mut list, active, TOGGLE);
            list.finish()
        };

        let on = draw(true);
        let [body, thumb] = on.commands() else {
            panic!("an active toggle must draw its body and its thumb");
        };
        assert!(matches!(
            body,
            DrawCmd::Fill {
                geom: Geom::Rect(rect),
                paint: Paint::Solid(color),
            } if *rect == TOGGLE && *color == skin.palette.accent
        ));
        assert!(matches!(
            thumb,
            DrawCmd::Fill {
                geom: Geom::Rect(Rect {
                    h: 9.0,
                    w: 9.0,
                    x: 15.0,
                    y: 2.5,
                }),
                paint: Paint::Solid(color),
            } if *color == skin.palette.bg_deep
        ));

        let off = draw(false);
        let [outline, thumb] = off.commands() else {
            panic!("an idle toggle must draw its outline and its thumb");
        };
        assert!(matches!(
            outline,
            DrawCmd::Stroke {
                geom: Geom::Rect(Rect {
                    h: 13.0,
                    w: 25.0,
                    x: 0.5,
                    y: 0.5,
                }),
                pen: Pen { width: 1.0, .. },
                ..
            }
        ));
        assert!(matches!(
            thumb,
            DrawCmd::Fill {
                geom: Geom::Rect(Rect { x: 2.0, .. }),
                paint: Paint::Solid(color),
            } if *color == skin.palette.muted
        ));
    }

    /// A checkbox is the same switch without the thumb. The thumb is the one
    /// part that does not span the box, so a checkbox must draw nothing that
    /// sits inside its own frame — whichever way it is set.
    #[kithara::test]
    fn a_checkbox_is_the_same_switch_without_a_thumb() {
        let skin = builtin::skin();
        let square = Rect {
            h: 14.0,
            w: 14.0,
            x: 0.0,
            y: 0.0,
        };
        let draw = |painter: &Binary, active| {
            let mut list = DrawListBuilder::default();
            painter.paint(&mut list, active, square);
            list.finish()
        };
        let inner = |list: &crate::draw::DrawList| {
            list.commands()
                .iter()
                .filter(|command| {
                    let (DrawCmd::Fill {
                        geom: Geom::Rect(rect),
                        ..
                    }
                    | DrawCmd::Stroke {
                        geom: Geom::Rect(rect),
                        ..
                    }) = command
                    else {
                        return false;
                    };
                    rect.w < square.w - 2.0
                })
                .count()
        };

        for active in [false, true] {
            assert_eq!(
                inner(&draw(&Binary::checkbox(skin), active)),
                0,
                "a checkbox must draw the switch and no thumb"
            );
            assert_eq!(
                inner(&draw(&Binary::toggle(skin), active)),
                1,
                "a toggle must draw exactly one thumb inside its box"
            );
        }
        assert_ne!(
            draw(&Binary::checkbox(skin), true),
            draw(&Binary::checkbox(skin), false),
            "the same checkbox must draw differently once it is ticked"
        );
    }
}
