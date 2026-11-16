//! The same admitted, caller-owned animation through both shipped hosts.

use std::cell::Cell;

use iced::Theme;
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    capture::{Geometry, Offscreen, Photographer},
    compile::compile,
    draw::Rect,
    ids::SourceUri,
    registry::{EndpointCategory, EndpointDesc, MapEndpoints, ValueKind},
    render::{Clock, ReadValue, Reads, Skin, UiEvent, tree},
    shaping::FontPolicy,
    skin::parse_skin_over,
    source::UiConfig,
    view,
};
use num_traits::ToPrimitive;
use vello::peniko::color::palette::css::BLACK;

const WIDTH: u32 = 96;
const HEIGHT: u32 = 128;
const ARTWORK: &[u8] = include_bytes!("../fixtures/artwork.json");

struct Picture {
    skin: Skin,
    seconds: Cell<f64>,
}

impl Reads for Picture {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        (endpoint == "fixture.time").then(|| ReadValue::Scalar(self.seconds.get()))
    }
}

impl App for Picture {
    fn document(&self) -> &str {
        "layout.ron"
    }
    fn skin(&self) -> &Skin {
        &self.skin
    }
    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }
    fn update(&mut self, _: UiEvent) {}
}

fn crop(pixels: &[u8], rect: Rect, scale: usize) -> Vec<u8> {
    assert_eq!((rect.w, rect.h), (64.0, 64.0));
    let x = rect
        .x
        .round()
        .to_usize()
        .expect("crop x must be on the surface");
    let y = rect
        .y
        .round()
        .to_usize()
        .expect("crop y must be on the surface");
    let width = usize::try_from(WIDTH).expect("surface width fits usize") * scale;
    (0..64)
        .flat_map(|row| {
            (0..64).flat_map(move |col| {
                let row = (y + row) * scale + scale / 2;
                let col = (x + col) * scale + scale / 2;
                let start = (row * width + col) * 4;
                pixels[start..start + 4].iter().copied()
            })
        })
        .collect()
}

fn coloured(pixels: &[u8], x: usize, y: usize) -> bool {
    let at = (y * 64 + x) * 4;
    pixels[at..at + 3].iter().any(|&channel| channel != 0)
}

fn compare(left: &[u8], right: &[u8]) {
    assert_eq!(left.len(), 64 * 64 * 4);
    assert_eq!(left.len(), right.len());
    let bounds = |pixels: &[u8]| {
        let points: Vec<_> = (0..64)
            .flat_map(|y| (0..64).map(move |x| (x, y)))
            .filter(|&(x, y)| coloured(pixels, x, y))
            .collect();
        assert!(
            points.len() > 100,
            "the admitted artwork must draw actual colour"
        );
        [
            points
                .iter()
                .map(|p| p.0)
                .min()
                .expect("the artwork has coloured pixels"),
            points
                .iter()
                .map(|p| p.1)
                .min()
                .expect("the artwork has coloured pixels"),
            points
                .iter()
                .map(|p| p.0)
                .max()
                .expect("the artwork has coloured pixels"),
            points
                .iter()
                .map(|p| p.1)
                .max()
                .expect("the artwork has coloured pixels"),
        ]
    };
    for (left, right) in bounds(left).into_iter().zip(bounds(right)) {
        assert!(
            left.abs_diff(right) <= 1,
            "host outlines differ by more than one pixel"
        );
    }
    // The rasterisers may cover one outline pixel differently; interior paint is exact to RGBA8.
    for y in 1..63 {
        for x in 1..63 {
            let inside = coloured(left, x, y);
            if [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
                .into_iter()
                .all(|(x, y)| coloured(left, x, y) == inside)
            {
                let at = (y * 64 + x) * 4;
                assert!(
                    left[at..at + 4]
                        .iter()
                        .zip(&right[at..at + 4])
                        .all(|(&a, &b)| a.abs_diff(b) <= 1),
                    "interior paint differs at {x},{y}: retained {:?}, immediate {:?}",
                    &left[at..at + 4],
                    &right[at..at + 4]
                );
            }
        }
    }
}

#[kithara::test]
fn caller_owned_animation_agrees_between_immediate_and_retained_hosts() {
    let mut resolver = builtin::resolver();
    resolver.insert_bytes("skin/picture.json", ARTWORK);
    resolver.insert(
        "layout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "picture",
        root: Module(instance: "demo", source: "module.ron"))"#,
    );
    resolver.insert(
        "module.ron",
        r#"(schema: "kithara.module", version: 1, id: "picture", chrome: Plain,
        root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Lottie(id: "picture", artwork: "caller", seconds: 1.0,
                read: Model(id: "fixture.time"), size: Some((w: Fixed(64.0), h: Fixed(64.0))))]))"#,
    );
    let mut endpoints = MapEndpoints::default();
    endpoints.insert(
        EndpointCategory::Model,
        "fixture.time",
        EndpointDesc::new(ValueKind::Scalar),
    );
    let origin = SourceUri("skin/skin.ron".into());
    let document = parse_skin_over(
        builtin::skin_doc(),
        r#"(schema: "kithara.skin", version: 1,
        id: "caller", artworks: {"caller": "picture.json"})"#,
        &origin,
    )
    .unwrap();
    let skin = Skin::resolve_with_font_policy(
        document,
        builtin::text_doc(),
        &origin,
        &resolver,
        FontPolicy::Embedded,
    )
    .unwrap();
    let compiled = compile(
        "layout.ron",
        &resolver,
        &endpoints,
        skin.document(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap();
    let mut ui = Ui::new(
        Picture {
            skin,
            seconds: Cell::new(0.0),
        },
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        (WIDTH * 2, HEIGHT * 2),
        2.0,
    )
    .unwrap();
    let mut photo = Photographer::new().unwrap();
    // Vello samples gradients at integer pixels; 2x odd pixels share Iced's logical centres.
    let mut capture = Offscreen::new(WIDTH * 2, HEIGHT * 2).unwrap();
    let mut previous = None;
    for seconds in [0.0, 0.5, 0.75] {
        ui.app().seconds.set(seconds);
        ui.frame(Duration::from_millis(16));
        let frame = ui.render().unwrap();
        let rect = ui.rect_of("demo/picture").unwrap();
        ui.complete_frame();
        let mut retained = Vec::new();
        capture
            .rasterise(&frame, 2.0, BLACK, &mut retained)
            .unwrap();
        let element = tree::render(
            &compiled.root,
            &compiled,
            ui.app(),
            &view::EMPTY,
            ui.app().skin(),
            Clock::default(),
            None,
        );
        let immediate = photo
            .shoot(
                element,
                &Theme::Dark,
                Geometry {
                    width: WIDTH,
                    height: HEIGHT,
                    scale: 1.0,
                },
            )
            .unwrap();
        let retained = crop(&retained, rect, 2);
        compare(&retained, &crop(&immediate, rect, 1));
        if let Some(previous) = previous {
            assert_ne!(retained, previous, "the next authored frame must move");
        }
        previous = Some(retained);
    }
}
