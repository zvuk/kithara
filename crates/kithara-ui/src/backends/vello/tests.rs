use kithara_test_utils::kithara;
use vello::{
    Scene,
    kurbo::{Cap, Join},
};

use super::backend::*;
use crate::{
    draw::{DrawCmd, DrawListBuilder, LineCap, LineJoin, Pen, Pt, Rect, Rgba, Transform, replay},
    shaping::{FontPolicy, GlyphFace, GlyphRun, TextContext, TextResources},
    skin::{ColorRole, FontFamily, FontWeight, TextRoleSkin},
};

#[kithara::test]
fn every_draw_operation_adds_to_the_encoding() {
    let run = TextContext::new()
        .unwrap()
        .shape("GAIN", FIXTURE.role, Some(FIXTURE.bounds.w));
    let mut builder = DrawListBuilder::default();
    builder.fill_circle(FIXTURE.point, 5.0, FIXTURE.color);
    builder.stroke_arc(FIXTURE.point, 5.0, 0.0, 1.0, FIXTURE.color, 1.0);
    builder.stroke_circle(FIXTURE.point, 5.0, FIXTURE.color, 1.0);
    builder.stroke_line(FIXTURE.point, Pt { x: 8.0, y: 8.0 }, FIXTURE.color, 1.0);
    builder.fill_rect(FIXTURE.bounds, FIXTURE.color);
    builder.fill_rounded_rect(FIXTURE.bounds, 3.0, FIXTURE.color);
    builder.stroke_rounded_rect(FIXTURE.bounds, 3.0, FIXTURE.color, 1.0);
    builder.text(&run, "GAIN", Transform::IDENTITY, FIXTURE.color);
    let list = builder.finish();
    let mut scene = Scene::new();

    replay(&list, &mut VelloBackend::new(&mut scene));

    assert_eq!(scene.encoding().n_paths, 7);
    assert!(!scene.encoding().resources.glyphs.is_empty());
}

#[kithara::test]
fn clip_replay_balances_the_layer_and_encodes_nested_commands() {
    let mut nested = DrawListBuilder::default();
    nested.fill_rect(FIXTURE.bounds, FIXTURE.color);
    let mut builder = DrawListBuilder::default();
    builder.clip(FIXTURE.bounds, nested.finish());
    let mut scene = Scene::new();

    replay(&builder.finish(), &mut VelloBackend::new(&mut scene));

    assert_eq!(scene.encoding().n_clips, 2);
    assert_eq!(scene.encoding().n_open_clips, 0);
    assert_eq!(scene.encoding().n_paths, 3);
}

#[kithara::test]
fn system_text_detection_covers_direct_and_nested_clips_only() {
    let system = system_run();
    let mut direct = DrawListBuilder::default();
    direct.text(&system, "fallback", Transform::IDENTITY, FIXTURE.color);
    let direct = direct.finish();
    assert!(has_system_text(&direct));

    let mut nested = DrawListBuilder::default();
    nested.clip(FIXTURE.bounds, direct);
    assert!(has_system_text(&nested.finish()));

    let embedded = TextContext::new()
        .unwrap_or_else(|error| panic!("embedded text context must build: {error}"))
        .shape("GAIN", FIXTURE.role, None);
    let mut embedded_list = DrawListBuilder::default();
    embedded_list.text(&embedded, "GAIN", Transform::IDENTITY, FIXTURE.color);
    assert!(!has_system_text(&embedded_list.finish()));
}

#[kithara::test]
fn stroke_width_changes_the_encoding() {
    let thin = line_scene(1.0);
    let thick = line_scene(3.0);

    assert_ne!(thin.encoding().styles, thick.encoding().styles);
}

#[kithara::test]
fn a_plain_pen_cuts_its_ends_flush_and_its_corners_to_a_point() {
    let stroke = stroke(Pen::new(2.0));

    assert_eq!(stroke.start_cap, Cap::Butt);
    assert_eq!(stroke.end_cap, Cap::Butt);
    assert_eq!(stroke.join, Join::Miter);
}

/// Every shape a pen can take reaches Vello as the shape it named.
#[kithara::test]
fn a_shaped_pen_keeps_its_shape_through_the_backend() {
    for (cap, expected) in [
        (LineCap::Butt, Cap::Butt),
        (LineCap::Round, Cap::Round),
        (LineCap::Square, Cap::Square),
    ] {
        let stroke = stroke(Pen::new(2.0).with_cap(cap));
        assert_eq!(stroke.start_cap, expected, "cap {cap:?}");
        assert_eq!(stroke.end_cap, expected, "cap {cap:?}");
    }
    for (join, expected) in [
        (LineJoin::Bevel, Join::Bevel),
        (LineJoin::Miter, Join::Miter),
        (LineJoin::Round, Join::Round),
    ] {
        assert_eq!(
            stroke(Pen::new(2.0).with_join(join)).join,
            expected,
            "join {join:?}"
        );
    }
}

#[kithara::test]
fn text_content_adds_glyphs() {
    let no_text = text_scene("");
    let text = text_scene("GAIN");

    assert!(no_text.encoding().resources.glyphs.is_empty());
    assert!(!text.encoding().resources.glyphs.is_empty());
}

#[kithara::test]
fn missing_character_does_not_drop_any_positioned_glyphs() {
    let scene = text_scene("A\u{10ffff}B");

    assert_eq!(scene.encoding().resources.glyphs.len(), 3);
}

fn line_scene(width: f32) -> Scene {
    let mut builder = DrawListBuilder::default();
    builder.stroke_line(FIXTURE.point, Pt { x: 8.0, y: 8.0 }, FIXTURE.color, width);
    let mut scene = Scene::new();
    replay(&builder.finish(), &mut VelloBackend::new(&mut scene));
    scene
}

fn text_scene(content: &str) -> Scene {
    let run = TextContext::new()
        .unwrap()
        .shape(content, FIXTURE.role, Some(FIXTURE.bounds.w));
    let mut builder = DrawListBuilder::default();
    builder.text(
        &run,
        content,
        Transform::translate(Pt {
            x: FIXTURE.bounds.x + (FIXTURE.bounds.w - run.width()) / 2.0,
            y: FIXTURE.bounds.y,
        }),
        FIXTURE.color,
    );
    let mut scene = Scene::new();
    replay(&builder.finish(), &mut VelloBackend::new(&mut scene));
    scene
}

fn system_run() -> GlyphRun {
    let resources = TextResources::new(FontPolicy::System)
        .unwrap_or_else(|error| panic!("system text resources must build: {error}"));
    let mut text = TextContext::from(&resources);
    for content in ["曲名", "שלום", "مرحبا", "ಜಗ", "ชื่อ"] {
        let run = text.shape(content, FIXTURE.role, None);
        if run
            .segments()
            .iter()
            .any(|segment| matches!(segment.face(), GlyphFace::System(_)))
        {
            return run;
        }
    }
    panic!("a system face must answer at least one script outside the embedded catalog");
}

#[derive(Clone, Copy)]
struct DrawFixture {
    point: Pt,
    bounds: Rect,
    color: Rgba,
    role: TextRoleSkin,
}

const FIXTURE: DrawFixture = {
    let color = Rgba {
        a: 1.0,
        b: 0.25,
        g: 0.5,
        r: 0.75,
    };
    DrawFixture {
        color,
        bounds: Rect {
            h: 12.0,
            w: 40.0,
            x: 0.0,
            y: 0.0,
        },
        point: Pt { x: 4.0, y: 4.0 },
        role: TextRoleSkin {
            color: ColorRole::Text,
            font: FontFamily::Sans,
            size: 12.0,
            spacing: 0.0,
            weight: FontWeight::Normal,
        },
    }
};

#[cfg(feature = "render")]
#[kithara::test]
fn replaying_a_knob_list_encodes_one_path_per_geometry_command() {
    const BOUNDS: Rect = Rect {
        h: 39.0,
        w: 28.0,
        x: 0.0,
        y: 0.0,
    };

    let mut builder = DrawListBuilder::default();
    crate::atoms::knob::Knob::new(crate::builtin::skin()).paint(
        &mut builder,
        &mut TextContext::new().unwrap(),
        0.25,
        Some("GAIN"),
        BOUNDS,
    );
    let list = builder.finish();
    let geometry = list
        .commands()
        .iter()
        .filter(|command| !matches!(command, DrawCmd::Text { .. }))
        .fold(0_u32, |count, _| count + 1);

    let mut scene = Scene::new();
    replay(&list, &mut VelloBackend::new(&mut scene));

    assert_eq!(scene.encoding().n_paths, geometry);
    assert!(!scene.encoding().resources.glyphs.is_empty());
}
