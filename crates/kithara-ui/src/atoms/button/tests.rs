use kithara_test_utils::kithara;

use super::face::*;
use crate::{
    builtin,
    draw::{DrawCmd, DrawListBuilder, Geom, Paint, Pen, Rect, Rgba},
    ids::SourceUri,
    layout::FrameSides,
    module::ButtonStyle,
    render::{Mark, Skin},
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
    let document = parse_skin_over(builtin::skin_doc(), text, &origin).expect("the patch parses");
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
