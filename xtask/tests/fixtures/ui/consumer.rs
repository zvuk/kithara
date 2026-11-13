use std::{collections::BTreeMap, fs, sync::Arc};

use kithara_ui_draw::{
    Backend, Caps, DrawList, DrawListBuilder, Geom, Image, Needs, Paint, Pen, Pt, Rect, Rgba, Stop,
    Stops, Transform, Unsupported, replay,
};
use kithara_ui_shaping::{
    FontFamily, FontPolicy, FontWeight, GlyphFace, GlyphRun, TextContext, TextError, TextResources,
    TextStyle,
};
use skrifa::{FontRef, MetadataProvider};

const FONT: &str = env!(
    "EXTERNAL_UI_FONT",
    "set EXTERNAL_UI_FONT to the supplied font file"
);
const INK: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const BOX: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 10.0,
    h: 10.0,
};
const STYLE: TextStyle = TextStyle {
    font: FontFamily::Sans,
    weight: FontWeight::Normal,
    size: 32.0,
    spacing: 0.0,
};

#[derive(Default)]
struct Renderer {
    calls: usize,
    runs: Vec<GlyphRun>,
}

impl Backend for Renderer {
    const CAPS: Caps = Caps {
        linear_gradient: false,
        ..Caps::EVERYTHING
    };

    fn clip(&mut self, _: Rect, _: &DrawList) {
        self.calls += 1;
    }
    fn fill(&mut self, _: &Geom, _: Paint) {
        self.calls += 1;
    }
    fn image(&mut self, _: &Image, _: Rect, _: f32) {
        self.calls += 1;
    }
    fn stroke(&mut self, _: &Geom, _: Rgba, _: Pen) {
        self.calls += 1;
    }

    fn text(&mut self, run: &GlyphRun, _: &str, _: Transform, _: Rgba) {
        self.calls += 1;
        self.runs.push(run.clone());
    }
}

fn public_gradient_refusal_precedes_every_backend_callback() {
    let stops = Stops::new(&[
        Stop {
            offset: 0.0,
            color: INK,
        },
        Stop {
            offset: 1.0,
            color: Rgba { r: 1.0, ..INK },
        },
    ])
    .unwrap();
    let mut list = DrawListBuilder::default();
    list.fill_rect(BOX, INK);
    list.fill_rect(
        BOX,
        Paint::Linear {
            from: Pt { x: 0.0, y: 0.0 },
            to: Pt { x: 10.0, y: 0.0 },
            stops,
        },
    );
    let list = list.finish();
    assert_eq!(
        Renderer::CAPS.accepts(Needs::from(&list)),
        Err(Unsupported::LinearGradient)
    );
    let mut renderer = Renderer::default();
    replay(&list, &mut renderer);
    assert_eq!(renderer.calls, 0);
}

fn invalid_caller_bytes_have_a_typed_admission_error() {
    assert!(matches!(
        TextResources::with_fonts(
            FontPolicy::Embedded,
            BTreeMap::from([(FontFamily::Sans, vec![Arc::from(b"not a font".as_slice())],)])
        ),
        Err(TextError::InvalidSource {
            family: FontFamily::Sans,
            ..
        })
    ));
}

fn supplied_face_and_positions_survive_ownership_drop_and_replay() {
    let bytes = fs::read(FONT).unwrap();
    let mut context = {
        let resources = TextResources::with_fonts(
            FontPolicy::Embedded,
            BTreeMap::from([(FontFamily::Sans, vec![Arc::from(bytes.clone())])]),
        )
        .unwrap();
        TextContext::from(&resources)
    };
    let words = "\u{0426}\u{0430}\u{0440}\u{044c} \u{0412}\u{043e}\u{043b}\u{043d}\u{044b}";
    let run = context.shape(words, STYLE, None);
    drop(context);
    let font = FontRef::from_index(&bytes, 0).unwrap();
    let expected_ids: Vec<_> = words
        .chars()
        .map(|ch| font.charmap().map(ch).unwrap().to_u32())
        .collect();
    let ids: Vec<_> = run
        .segments()
        .iter()
        .flat_map(|segment| segment.glyphs().iter().map(|glyph| glyph.id))
        .collect();
    assert_eq!(ids, expected_ids);
    assert!(ids.iter().all(|id| *id != 0));
    assert!(run.width() > 0.0 && run.height() > 0.0);
    for segment in run.segments() {
        let GlyphFace::System(font) = segment.face() else {
            panic!("the caller's face must answer");
        };
        assert_eq!(font.data.as_ref(), bytes);
    }
    let default = TextContext::new().unwrap().shape(words, STYLE, None);
    assert_ne!(run.width(), default.width());
    let mut list = DrawListBuilder::default();
    list.text(
        &run,
        "this string must never replace the glyph data",
        Transform::translate(Pt { x: 20.0, y: 60.0 }),
        INK,
    );
    let mut renderer = Renderer::default();
    replay(&list.finish(), &mut renderer);
    assert_eq!(renderer.calls, 1);
    assert_eq!(renderer.runs, [run]);
}

#[cfg(feature = "vello")]
fn production_vello_keeps_supplied_font_and_shaped_glyph_positions() {
    let bytes = fs::read(FONT).unwrap();
    let resources = TextResources::with_fonts(
        FontPolicy::Embedded,
        BTreeMap::from([(FontFamily::Sans, vec![Arc::from(bytes.clone())])]),
    )
    .unwrap();
    let run = TextContext::from(&resources).shape("\u{0426}\u{0430}\u{0440}\u{044c}", STYLE, None);
    let mut list = DrawListBuilder::default();
    list.text(&run, "do not reshape this", Transform::IDENTITY, INK);
    let mut scene = vello::Scene::new();
    replay(
        &list.finish(),
        &mut kithara_ui::backends::VelloBackend::new(&mut scene),
    );
    let encoded = &scene.encoding().resources;
    assert!(!encoded.glyph_runs.is_empty());
    assert!(
        encoded
            .glyph_runs
            .iter()
            .all(|glyph_run| glyph_run.font.data.as_ref() == bytes)
    );
    let positioned: Vec<_> = run
        .segments()
        .iter()
        .flat_map(|segment| segment.glyphs())
        .collect();
    assert_eq!(encoded.glyphs.len(), positioned.len());
    for (painted, shaped) in encoded.glyphs.iter().zip(positioned) {
        assert_eq!(
            (painted.id, painted.x, painted.y),
            (shaped.id, shaped.x, shaped.y)
        );
    }
}

fn main() {
    public_gradient_refusal_precedes_every_backend_callback();
    invalid_caller_bytes_have_a_typed_admission_error();
    supplied_face_and_positions_survive_ownership_drop_and_replay();
    #[cfg(feature = "vello")]
    production_vello_keeps_supplied_font_and_shaped_glyph_positions();
}
