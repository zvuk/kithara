use crate::{
    atoms::icon::mark::Marked,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    layout::FrameSides,
    module::ButtonStyle,
    render::{Mark, Skin},
    shaping::{GlyphRun, TextContext},
    skin::{ColorRole, FontFamily, FontSkin, FrameSkin, TextRoleSkin},
    solve::{Length, Size},
};

/// What a document asks a button to be, before a skin resolves it.
#[derive(bon::Builder, Clone, Copy)]
pub(crate) struct ButtonConfig {
    frame: Option<FrameSides>,
    mark: Option<Mark>,
    style: ButtonStyle,
}

/// The word a button shows, and the word it swaps in while it is active. A
/// host that draws straight through borrows them; one that retains the control
/// owns them.
#[derive(Clone, Copy)]
pub(crate) struct ButtonLabel<Words> {
    pub(crate) active: Option<Words>,
    pub(crate) label: Words,
}

pub(crate) struct Button {
    active: Face,
    idle: Face,
    width: Width,
}

/// What settles a button's width: a share of the row it sits in, a number the
/// skin fixes, or the word it is currently showing.
#[derive(Clone, Copy)]
enum Width {
    Content,
    Fixed(f32),
    Portion(u16),
}

/// How the button looks in one of its two states. Both are resolved from the
/// skin when the button is built, so the endpoint behind it flipping is a
/// repaint rather than a reason to rebuild the control.
struct Face {
    active: bool,
    art: Option<Art>,
    content: Rgba,
    fill: Fill,
    frame: Frame,
    gap: f32,
    padding_x: f32,
    role: TextRoleSkin,
}

#[derive(Clone, Copy)]
pub(crate) enum VisualState {
    Idle,
    Hovered,
    Pressed,
}

#[derive(Clone, Copy)]
struct Fill {
    hovered: Rgba,
    idle: Rgba,
    pressed: Rgba,
}

enum Frame {
    Border {
        color: Rgba,
        radius: f32,
        width: f32,
    },
    Seams {
        color: Rgba,
        sides: FrameSides,
        width: f32,
    },
}

/// The icon a button shows: what it draws, and where it sits.
struct Art {
    marked: Marked,
    placement: Placement,
    solo_color: Rgba,
}

#[derive(Clone, Copy)]
enum Placement {
    Alone,
    AloneIfUnlabelled,
    Beside,
}

impl Art {
    delegate::delegate! {
        to self.marked {
            fn width(&self, text: &mut TextContext) -> f32;
            fn paint(
                &self,
                list: &mut DrawListBuilder,
                text: &mut TextContext,
                x: f32,
                bounds: Rect,
                color: Rgba,
            ) -> f32;
        }
    }
}

impl Button {
    /// `config` describes the button at rest. `active_mark` is the icon it
    /// swaps in while active, for the styles whose icon changes with the state.
    pub(crate) fn new(config: ButtonConfig, active_mark: Option<Mark>, skin: &Skin) -> Self {
        Self {
            active: Face::new(
                ButtonConfig {
                    mark: active_mark,
                    ..config
                },
                true,
                skin,
            ),
            idle: Face::new(config, false, skin),
            width: Width::new(config.style, skin),
        }
    }

    /// The box it asks for. Only the width is its own: every button fills the
    /// height of the row it sits in.
    pub(crate) fn declared<Words>(
        &self,
        text: &mut TextContext,
        label: &ButtonLabel<Words>,
        active: bool,
    ) -> Size<Length>
    where
        Words: AsRef<str>,
    {
        let width = match self.width {
            Width::Content => Length::Fixed(self.intrinsic_width(text, label, active)),
            Width::Fixed(value) => Length::Fixed(value),
            Width::Portion(factor) => Length::FillPortion(factor),
        };
        Size::new(width, Length::Fill)
    }

    pub(crate) fn paint<Words>(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &ButtonLabel<Words>,
        active: bool,
        bounds: Rect,
        state: VisualState,
    ) where
        Words: AsRef<str>,
    {
        self.face(active).paint(list, text, label, bounds, state);
    }

    pub(crate) fn intrinsic_width<Words>(
        &self,
        text: &mut TextContext,
        label: &ButtonLabel<Words>,
        active: bool,
    ) -> f32
    where
        Words: AsRef<str>,
    {
        self.face(active).intrinsic_width(text, label)
    }

    const fn face(&self, active: bool) -> &Face {
        if active { &self.active } else { &self.idle }
    }
}

impl Width {
    fn new(style: ButtonStyle, skin: &Skin) -> Self {
        match style {
            ButtonStyle::Default => Self::Content,
            ButtonStyle::MicroPrimary => Self::Fixed(skin.button.micro_size),
            ButtonStyle::Transport => Self::Portion(skin.button.transport_fill),
            ButtonStyle::TransportPrimary => Self::Portion(skin.button.primary_fill),
            ButtonStyle::VisNav => Self::Fixed(skin.vis.nav_cell_size),
        }
    }
}

/// What a parent has to be told about a button's width before the button
/// exists.
///
/// A retained host settles a row's shares while it is still walking the
/// document, which is earlier than it holds a painter — so this reads the same
/// table the painter reads rather than restating it. A width the button
/// measures from its own word is not something a parent can be told, so it
/// answers `Shrink` and the leaf is asked instead.
///
/// Only the retained host asks: the immediate one reads the box off the built
/// widget, which by then holds the painter.
#[cfg(feature = "masonry")]
pub(crate) fn declared_width(style: ButtonStyle, skin: &Skin) -> Length {
    match Width::new(style, skin) {
        Width::Content => Length::Shrink,
        Width::Fixed(value) => Length::Fixed(value),
        Width::Portion(factor) => Length::FillPortion(factor),
    }
}

impl Face {
    fn new(config: ButtonConfig, active: bool, skin: &Skin) -> Self {
        let ButtonConfig { frame, mark, style } = config;
        let palette = skin.palette;
        let transport = matches!(
            style,
            ButtonStyle::Transport | ButtonStyle::TransportPrimary
        );
        let highlighted = active || style == ButtonStyle::MicroPrimary;
        let content: Rgba = if style == ButtonStyle::VisNav {
            skin.rgba(skin.vis.nav_text_color)
        } else if highlighted {
            palette.bg
        } else {
            palette.text
        };
        let font: FontSkin = if primary(style) || active {
            skin.button.primary_text
        } else if style == ButtonStyle::VisNav {
            skin.vis.nav_text
        } else {
            skin.button.text
        };
        Self {
            active,
            content,
            fill: fill(style, highlighted, transport, skin),
            frame: if transport {
                Frame::Seams {
                    color: skin.rgba(skin.divider.color),
                    sides: frame.unwrap_or(skin.button.transport_sides),
                    width: skin.divider.width,
                }
            } else {
                border(style, skin)
            },
            gap: skin.button.icon_gap,
            art: mark.map(|mark| Art {
                marked: Marked::new(mark, icon_size(style, transport, skin)),
                placement: placement(style, transport),
                solo_color: if transport && !highlighted {
                    palette.text_dim
                } else {
                    content
                },
            }),
            padding_x: if style == ButtonStyle::VisNav {
                skin.vis.nav_padding_x
            } else {
                skin.button.padding_x
            },
            role: TextRoleSkin {
                color: ColorRole::Text,
                font: FontFamily::Mono,
                size: font.size,
                spacing: 0.0,
                weight: font.weight,
            },
        }
    }

    fn paint<Words>(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &ButtonLabel<Words>,
        bounds: Rect,
        state: VisualState,
    ) where
        Words: AsRef<str>,
    {
        list.fill_rounded_rect(bounds, self.frame.radius(), self.fill.pick(state));
        self.frame.paint(list, bounds);
        self.paint_content(list, text, self.label(label), bounds);
    }

    fn intrinsic_width<Words>(&self, text: &mut TextContext, label: &ButtonLabel<Words>) -> f32
    where
        Words: AsRef<str>,
    {
        let label = self.label(label);
        let content = match &self.art {
            Some(art) => {
                let icon = art.width(text);
                if art.placement.alone(label) {
                    icon
                } else {
                    icon + self.gap + self.shape(text, label).width()
                }
            }
            None if label.is_empty() => 0.0,
            None => self.shape(text, label).width(),
        };
        content + self.padding_x * 2.0
    }

    fn paint_content(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &str,
        bounds: Rect,
    ) {
        let Some(art) = &self.art else {
            if !label.is_empty() {
                let run = self.shape(text, label);
                Self::paint_run(list, &run, label, bounds, self.content);
            }
            return;
        };
        let icon = art.width(text);
        if art.placement.alone(label) {
            art.paint(
                list,
                text,
                bounds.x + (bounds.w - icon) / 2.0,
                bounds,
                art.solo_color,
            );
            return;
        }

        let run = self.shape(text, label);
        let width = icon + self.gap + run.width();
        let x = bounds.x + (bounds.w - width) / 2.0;
        art.paint(list, text, x, bounds, self.content);
        if !label.is_empty() {
            list.text(
                &run,
                label,
                Transform::translate(Pt {
                    x: x + icon + self.gap,
                    y: bounds.y + (bounds.h - run.height()) / 2.0,
                }),
                self.content,
            );
        }
    }

    fn paint_run(
        list: &mut DrawListBuilder,
        run: &GlyphRun,
        content: &str,
        bounds: Rect,
        color: Rgba,
    ) {
        list.text(
            run,
            content,
            Transform::translate(Pt {
                x: bounds.x + (bounds.w - run.width()) / 2.0,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            color,
        );
    }

    fn shape(&self, text: &mut TextContext, label: &str) -> GlyphRun {
        text.shape(label, self.role, None)
    }

    fn label<'a, Words>(&self, label: &'a ButtonLabel<Words>) -> &'a str
    where
        Words: AsRef<str>,
    {
        if self.active {
            label
                .active
                .as_ref()
                .map_or_else(|| label.label.as_ref(), AsRef::as_ref)
        } else {
            label.label.as_ref()
        }
    }
}

impl Fill {
    const fn pick(self, state: VisualState) -> Rgba {
        match state {
            VisualState::Hovered => self.hovered,
            VisualState::Idle => self.idle,
            VisualState::Pressed => self.pressed,
        }
    }
}

impl Frame {
    const fn radius(&self) -> f32 {
        match self {
            Self::Border { radius, .. } => *radius,
            Self::Seams { .. } => 0.0,
        }
    }

    fn paint(&self, list: &mut DrawListBuilder, bounds: Rect) {
        match self {
            Self::Border {
                color,
                radius,
                width,
            } => {
                if *width <= 0.0 {
                    return;
                }
                let inset = width / 2.0;
                list.stroke_rounded_rect(
                    Rect {
                        h: (bounds.h - width).max(0.0),
                        w: (bounds.w - width).max(0.0),
                        x: bounds.x + inset,
                        y: bounds.y + inset,
                    },
                    *radius,
                    *color,
                    *width,
                );
            }
            Self::Seams {
                color,
                sides,
                width,
            } => Self::paint_seams(
                list,
                bounds,
                *sides,
                width.min(bounds.w).min(bounds.h),
                *color,
            ),
        }
    }

    fn paint_seams(
        list: &mut DrawListBuilder,
        bounds: Rect,
        sides: FrameSides,
        width: f32,
        color: Rgba,
    ) {
        if width <= 0.0 {
            return;
        }
        if sides.top {
            list.fill_rect(Rect { h: width, ..bounds }, color);
        }
        if sides.right {
            list.fill_rect(
                Rect {
                    h: bounds.h,
                    w: width,
                    x: bounds.x + bounds.w - width,
                    y: bounds.y,
                },
                color,
            );
        }
        if sides.bottom {
            list.fill_rect(
                Rect {
                    h: width,
                    w: bounds.w,
                    x: bounds.x,
                    y: bounds.y + bounds.h - width,
                },
                color,
            );
        }
        if sides.left {
            list.fill_rect(
                Rect {
                    h: bounds.h,
                    w: width,
                    x: bounds.x,
                    y: bounds.y,
                },
                color,
            );
        }
    }
}

impl Placement {
    const fn alone(self, label: &str) -> bool {
        match self {
            Self::Alone => true,
            Self::AloneIfUnlabelled => label.is_empty(),
            Self::Beside => false,
        }
    }
}

fn fill(style: ButtonStyle, highlighted: bool, transport: bool, skin: &Skin) -> Fill {
    let palette = skin.palette;
    if style == ButtonStyle::VisNav {
        return Fill {
            hovered: palette.bg_select,
            idle: skin.rgba(skin.vis.nav_background),
            pressed: palette.accent_soft,
        };
    }
    if highlighted {
        return Fill {
            hovered: palette.accent_strong,
            idle: palette.accent,
            pressed: palette.accent_soft,
        };
    }
    Fill {
        hovered: palette.bg_panel_2,
        idle: if transport {
            Rgba {
                a: 0.0,
                b: 0.0,
                g: 0.0,
                r: 0.0,
            }
        } else {
            palette.bg_panel
        },
        pressed: palette.accent_soft,
    }
}

fn border(style: ButtonStyle, skin: &Skin) -> Frame {
    let frame: FrameSkin = if style == ButtonStyle::VisNav {
        skin.vis.nav_frame
    } else if primary(style) {
        skin.button.primary_frame
    } else {
        skin.button.frame
    };
    Frame::Border {
        color: skin.rgba(frame.border),
        radius: frame.radius,
        width: frame.border_width,
    }
}

const fn placement(style: ButtonStyle, transport: bool) -> Placement {
    if matches!(style, ButtonStyle::MicroPrimary) {
        Placement::Alone
    } else if transport {
        Placement::AloneIfUnlabelled
    } else {
        Placement::Beside
    }
}

fn icon_size(style: ButtonStyle, transport: bool, skin: &Skin) -> f32 {
    if style == ButtonStyle::MicroPrimary {
        skin.button.micro_icon_size
    } else if transport {
        skin.button.transport_icon_size
    } else {
        skin.button.icon_size
    }
}

const fn primary(style: ButtonStyle) -> bool {
    matches!(
        style,
        ButtonStyle::TransportPrimary | ButtonStyle::MicroPrimary
    )
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, DrawListBuilder, Geom, Paint, Pen, Rect},
        module::ButtonStyle,
        shaping::{FontId, GlyphFace, GlyphSegment, TextContext},
    };

    fn plain(label: &str) -> ButtonLabel<&str> {
        ButtonLabel {
            active: None,
            label,
        }
    }

    #[kithara::test]
    fn a_default_button_draws_fill_border_and_label_in_order() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 30.0,
            w: 72.0,
            x: 0.0,
            y: 0.0,
        };
        let mut text = TextContext::from(skin.text_resources());
        let mut builder = DrawListBuilder::default();
        Button::new(
            ButtonConfig::builder().style(ButtonStyle::Default).build(),
            None,
            skin,
        )
        .paint(
            &mut builder,
            &mut text,
            &plain("DEFAULT"),
            false,
            bounds,
            VisualState::Idle,
        );
        let list = builder.finish();

        assert_eq!(list.commands().len(), 3);
        assert!(matches!(
            list.commands()[0],
            DrawCmd::Fill {
                geom: Geom::Rect(rect),
                paint: Paint::Solid(color),
            } if rect == bounds && color == skin.palette.bg_panel
        ));
        assert!(matches!(
            list.commands()[1],
            DrawCmd::Stroke {
                geom: Geom::Rect(_),
                color,
                pen: Pen { width: 1.0, .. },
            } if color == skin.palette.line
        ));
        assert!(matches!(
            &list.commands()[2],
            DrawCmd::Text { run, content, .. }
                if content == "DEFAULT"
                    && run.segments().first().map(GlyphSegment::face)
                        == Some(&GlyphFace::Embedded(FontId::JetBrainsMonoRegular))
        ));
    }

    #[kithara::test]
    fn a_micro_button_draws_its_lucide_glyph_through_the_text_command() {
        let skin = builtin::skin();
        let glyph = char::from(lucide_icons::Icon::Play);
        let mut text = TextContext::from(skin.text_resources());
        let mut builder = DrawListBuilder::default();
        Button::new(
            ButtonConfig::builder()
                .mark(Mark::Glyph(glyph))
                .style(ButtonStyle::MicroPrimary)
                .build(),
            Some(Mark::Glyph(glyph)),
            skin,
        )
        .paint(
            &mut builder,
            &mut text,
            &plain("PLAY"),
            false,
            Rect {
                h: 34.0,
                w: 34.0,
                x: 0.0,
                y: 0.0,
            },
            VisualState::Idle,
        );
        let list = builder.finish();

        assert!(matches!(
            &list.commands()[2],
            DrawCmd::Text { run, content, .. }
                if content == &glyph.to_string()
                    && run.segments().first().map(GlyphSegment::face)
                        == Some(&GlyphFace::Embedded(FontId::Lucide))
        ));
    }

    #[kithara::test]
    fn a_transport_button_draws_only_its_declared_seams() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 28.0,
            w: 48.0,
            x: 0.0,
            y: 0.0,
        };
        let mut text = TextContext::from(skin.text_resources());
        let mut builder = DrawListBuilder::default();
        Button::new(
            ButtonConfig::builder()
                .frame(FrameSides {
                    top: true,
                    right: false,
                    bottom: true,
                    left: false,
                })
                .style(ButtonStyle::TransportPrimary)
                .build(),
            None,
            skin,
        )
        .paint(
            &mut builder,
            &mut text,
            &plain("PLAY"),
            false,
            bounds,
            VisualState::Idle,
        );
        let list = builder.finish();

        assert!(matches!(
            list.commands()[0],
            DrawCmd::Fill {
                geom: Geom::Rect(rect),
                paint: Paint::Solid(color),
            } if rect == bounds && color.a == 0.0
        ));
        assert!(matches!(
            list.commands()[1],
            DrawCmd::Fill {
                geom: Geom::Rect(Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 48.0,
                    h: 1.0
                }),
                ..
            }
        ));
        assert!(matches!(
            list.commands()[2],
            DrawCmd::Fill {
                geom: Geom::Rect(Rect {
                    x: 0.0,
                    y: 27.0,
                    w: 48.0,
                    h: 1.0
                }),
                ..
            }
        ));
        assert!(matches!(list.commands()[3], DrawCmd::Text { .. }));
    }

    #[kithara::test]
    fn an_active_transport_button_uses_its_accent_and_active_label() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut builder = DrawListBuilder::default();
        Button::new(
            ButtonConfig::builder()
                .style(ButtonStyle::TransportPrimary)
                .build(),
            None,
            skin,
        )
        .paint(
            &mut builder,
            &mut text,
            &ButtonLabel {
                active: Some("PAUSE"),
                label: "PLAY",
            },
            true,
            Rect {
                h: 28.0,
                w: 48.0,
                x: 0.0,
                y: 0.0,
            },
            VisualState::Idle,
        );
        let list = builder.finish();

        assert!(matches!(
            list.commands()[0],
            DrawCmd::Fill { paint: Paint::Solid(color), .. } if color == skin.palette.accent
        ));
        assert!(matches!(
            list.commands().last(),
            Some(DrawCmd::Text { content, color, .. })
                if content == "PAUSE" && *color == skin.palette.bg
        ));
    }
}
