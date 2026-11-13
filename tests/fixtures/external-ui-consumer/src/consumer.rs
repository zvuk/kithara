use std::{collections::BTreeMap, fmt::Write, fs, sync::Arc};

use kithara_ui_draw::{
    Backend, Caps, DrawList, DrawListBuilder, Geom, Image, Needs, Paint, Pen, Pt, Rect, Rgba, Stop,
    Stops, Transform, Unsupported, replay,
};
use kithara_ui_shaping::{
    FontFamily, FontPolicy, FontWeight, GlyphFace, GlyphRun, TextContext, TextError, TextResources,
    TextStyle,
};
use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    instance::{LocationRef, NormalizedCoord, Size},
    outline::{DrawSettings, OutlineGlyphCollection, OutlinePen},
};

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
    path: String,
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

    fn text(&mut self, run: &GlyphRun, _: &str, transform: Transform, _: Rgba) {
        self.calls += 1;
        self.runs.push(run.clone());
        for segment in run.segments() {
            let (bytes, index) = match segment.face() {
                GlyphFace::Embedded(face) => (face.bytes(), 0),
                GlyphFace::System(font) => (font.data.as_ref(), font.index),
            };
            let font = FontRef::from_index(bytes, index).unwrap();
            let outlines = OutlineGlyphCollection::new(&font);
            let coords: Vec<_> = segment
                .normalized_coords()
                .iter()
                .map(|coord| NormalizedCoord::from_bits(*coord))
                .collect();
            for glyph in segment.glyphs() {
                let Some(outline) = outlines.get(GlyphId::new(glyph.id)) else {
                    continue;
                };
                let mut pen = SvgPen {
                    path: &mut self.path,
                    transform,
                    origin: Pt {
                        x: glyph.x,
                        y: glyph.y,
                    },
                };
                let settings =
                    DrawSettings::unhinted(Size::new(run.size()), LocationRef::new(&coords));
                outline.draw(settings, &mut pen).unwrap();
            }
        }
    }
}

struct SvgPen<'a> {
    path: &'a mut String,
    transform: Transform,
    origin: Pt,
}

impl SvgPen<'_> {
    fn point(&self, x: f32, y: f32) -> Pt {
        self.transform.apply(Pt {
            x: self.origin.x + x,
            y: self.origin.y - y,
        })
    }
    fn point_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        write!(self.path, "{} {} ", point.x, point.y).unwrap();
    }
}

impl OutlinePen for SvgPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.push('M');
        self.point_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.push('L');
        self.point_to(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.path.push('Q');
        self.point_to(cx, cy);
        self.point_to(x, y);
    }
    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        self.path.push('C');
        self.point_to(ax, ay);
        self.point_to(bx, by);
        self.point_to(x, y);
    }
    fn close(&mut self) {
        self.path.push('Z');
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

fn supplied_face_and_positions_survive_ownership_drop_and_svg_replay() {
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
    assert!(renderer.path.contains('M') && renderer.path.contains('Z'));
    fs::create_dir_all("target/acceptance").unwrap();
    fs::write("target/acceptance/supplied-roboto.svg", format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"400\" height=\"100\"><path d=\"{}\"/></svg>", renderer.path)).unwrap();
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
    supplied_face_and_positions_survive_ownership_drop_and_svg_replay();
    #[cfg(feature = "vello")]
    production_vello_keeps_supplied_font_and_shaped_glyph_positions();
}
