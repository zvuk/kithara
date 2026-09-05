use crate::{
    atoms::icon::mark::Marked,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    layout::FrameSides,
    module::ButtonStyle,
    render::{Mark, Skin},
    shaping::{GlyphRun, TextContext},
    skin::{FrameSkin, TextRoleSkin},
    solve::{Length, Size},
};

/// What a document asks a button to be, before a skin resolves it.
#[derive(bon::Builder, Clone, Copy)]
pub(crate) struct ButtonConfig {
    style: ButtonStyle,
    frame: Option<FrameSides>,
    mark: Option<Mark>,
}

/// The word a button shows, and the word it swaps in while it is active. A
/// host that draws straight through borrows them; one that retains the control
/// owns them.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct ButtonLabel<Words> {
    pub(crate) active: Option<Words>,
    pub(crate) label: Words,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Button {
    active: Face,
    idle: Face,
    width: Width,
}

/// What settles a button's width: the box the row hands it, a number the skin
/// fixes, or a share of the row it sits in.
#[derive(Clone, Copy, PartialEq)]
enum Width {
    Fill,
    Fixed(f32),
    Portion(u16),
}

/// How the button looks in one of its two states. Both are resolved from the
/// skin when the button is built, so the endpoint behind it flipping is a
/// repaint rather than a reason to rebuild the control.
#[derive(Clone, PartialEq)]
struct Face {
    fill: Fill,
    frame: Frame,
    art: Option<Art>,
    content: Rgba,
    role: TextRoleSkin,
    active: bool,
    gap: f32,
    padding_x: f32,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum VisualState {
    Idle,
    Hovered,
    Pressed,
}

#[derive(Clone, Copy, PartialEq)]
struct Fill {
    hovered: Rgba,
    idle: Rgba,
    pressed: Rgba,
}

#[derive(Clone, PartialEq)]
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
#[derive(Clone, PartialEq)]
struct Art {
    marked: Marked,
    placement: Placement,
    solo_color: Rgba,
}

#[derive(Clone, Copy, PartialEq)]
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
    pub(crate) fn declared(&self) -> Size<Length> {
        Size::new(self.width.length(), Length::Fill)
    }

    const fn face(&self, active: bool) -> &Face {
        if active { &self.active } else { &self.idle }
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
}

impl Width {
    fn new(style: ButtonStyle, skin: &Skin) -> Self {
        match style {
            ButtonStyle::Default => Self::Fill,
            ButtonStyle::MicroPrimary => Self::Fixed(skin.button.micro_size),
            ButtonStyle::Transport => Self::Portion(skin.button.transport_portion),
            ButtonStyle::TransportPrimary => Self::Portion(skin.button.primary_portion),
            ButtonStyle::VisNav => Self::Fixed(skin.vis.nav_cell_size),
        }
    }

    const fn length(self) -> Length {
        match self {
            Self::Fill => Length::Fill,
            Self::Fixed(value) => Length::Fixed(value),
            Self::Portion(factor) => Length::FillPortion(factor),
        }
    }
}

/// What a parent has to be told about a button's width before the button
/// exists.
///
/// A retained host settles a row's shares while it is still walking the
/// document, which is earlier than it holds a painter — so this reads the same
/// table the painter reads rather than restating it.
///
/// Only the retained host asks: the immediate one reads the box off the built
/// widget, which by then holds the painter.
#[cfg(feature = "masonry")]
pub(crate) fn declared_width(style: ButtonStyle, skin: &Skin) -> Length {
    Width::new(style, skin).length()
}

impl Face {
    fn new(config: ButtonConfig, active: bool, skin: &Skin) -> Self {
        let ButtonConfig { frame, mark, style } = config;
        let transport = matches!(
            style,
            ButtonStyle::Transport | ButtonStyle::TransportPrimary
        );
        let highlighted = active;
        let role: TextRoleSkin = if style == ButtonStyle::VisNav {
            skin.vis.nav_text
        } else if primary(style) || active {
            skin.button.primary_text
        } else {
            skin.button.text
        };
        let color = if style == ButtonStyle::VisNav {
            skin.vis.nav_text.color
        } else if highlighted {
            skin.button.primary_text.color
        } else if style == ButtonStyle::MicroPrimary {
            skin.button.dim_text_color
        } else {
            skin.button.text.color
        };
        let content: Rgba = skin.rgba(color);
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
                    skin.rgba(skin.button.dim_text_color)
                } else {
                    content
                },
            }),
            padding_x: if style == ButtonStyle::VisNav {
                skin.vis.nav_padding_x
            } else {
                skin.button.padding_x
            },
            role: TextRoleSkin { color, ..role },
        }
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

    const fn radius(&self) -> f32 {
        match self {
            Self::Border { radius, .. } => *radius,
            Self::Seams { .. } => 0.0,
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
    let colors = if style == ButtonStyle::VisNav {
        skin.vis.nav_fill
    } else if highlighted {
        skin.button.primary_fill
    } else if transport {
        skin.button.transport_fill
    } else {
        skin.button.fill
    };
    Fill {
        hovered: skin.tint(colors.hovered),
        idle: skin.tint(colors.idle),
        pressed: skin.tint(colors.pressed),
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
        ids::SourceUri,
        module::ButtonStyle,
        shaping::{FontId, GlyphFace, GlyphSegment, TextContext},
        skin::parse_skin_over,
    };

    fn plain(label: &str) -> ButtonLabel<&str> {
        ButtonLabel {
            label,
            active: None,
        }
    }

    /// The colour the micro play button paints its cell with, at rest.
    fn micro_fill(active: bool) -> Rgba {
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
            &plain(""),
            active,
            Rect {
                h: 34.0,
                w: 34.0,
                x: 0.0,
                y: 0.0,
            },
            VisualState::Idle,
        );
        let list = builder.finish();
        let Some(DrawCmd::Fill {
            paint: Paint::Solid(color),
            ..
        }) = list.commands().first()
        else {
            panic!("a button paints its cell first");
        };
        *color
    }

    #[kithara::test]
    fn a_playing_micro_button_takes_the_accent() {
        assert_eq!(micro_fill(true), builtin::skin().palette.accent);
    }

    #[kithara::test]
    fn a_stopped_micro_button_does_not_take_the_accent() {
        assert_ne!(micro_fill(false), builtin::skin().palette.accent);
    }

    fn idle_fill(skin: &Skin) -> Rgba {
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
        let Some(DrawCmd::Fill {
            paint: Paint::Solid(color),
            ..
        }) = list.commands().first()
        else {
            panic!("a button paints its cell first");
        };
        *color
    }

    #[kithara::test]
    fn a_button_takes_the_idle_fill_a_second_skin_writes_over_it() {
        let origin = SourceUri("loud.kskin.ron".to_owned());
        let text = r##"(
            schema: "kithara.skin",
            version: 1,
            id: "kithara-loud",
            button: (fill: (hovered: BgPanel2, idle: Danger, pressed: AccentSoft)),
        )"##;
        let document =
            parse_skin_over(builtin::skin_doc(), text, &origin).expect("the patch parses");
        let skin = Skin::resolve(document, builtin::text_doc(), &origin, &builtin::resolver())
            .expect("the patched document resolves");

        assert_eq!(idle_fill(&skin), skin.palette.danger);
    }

    #[kithara::test]
    fn a_button_the_skin_says_nothing_new_about_keeps_its_fill() {
        assert_eq!(idle_fill(builtin::skin()), builtin::skin().palette.bg_panel);
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
