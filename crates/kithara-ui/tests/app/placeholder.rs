#![cfg(feature = "gpu")]

//! A device picture on the capture host, which has no producer to bind it.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    capture::Offscreen,
    draw::{DrawListBuilder, ImageId, Rect, Rgba},
    registry::MapEndpoints,
    render::{
        CustomSkin, ReadValue, Reads, Skin, UiEvent, WindowCommand,
        custom::{CustomKinds, CustomWidget, Size2, SizeLimits, TextMeasurer},
        external::ExternalImageDeclaration,
    },
    source::MemResolver,
};
use vello::peniko::{Color, ImageAlphaType};

const RED: Rgba = Rgba {
    a: 1.0,
    r: 1.0,
    g: 0.0,
    b: 0.0,
};

/// A widget that fills its box red and draws a device picture over it.
struct Covered {
    declaration: ExternalImageDeclaration,
}

impl CustomWidget for Covered {
    type Action = ();

    fn external_image(&self, id: &ImageId) -> Option<&ExternalImageDeclaration> {
        (id == self.declaration.image().id()).then_some(&self.declaration)
    }

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(32.0, 32.0)
    }

    fn paint(
        &mut self,
        list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        bounds: Rect,
        _skin: &CustomSkin,
    ) {
        list.fill_rect(bounds, RED);
        list.image(self.declaration.image().clone(), bounds);
    }
}

/// An application with nothing to read: the document is one covered widget.
struct Still;

impl Reads for Still {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

impl App for Still {
    fn document(&self) -> &str {
        "one.klayout.ron"
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn update(&mut self, _event: UiEvent) {}
}

fn documents() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "one.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "one",
            root: Module(instance: "demo", source: "one.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "one.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "covered", chrome: Plain,
            root: Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Custom(id: "preview", kind: "fixture-covered", size: Some((w: Fill, h: Fill))),
            ]))"#,
    );
    resolver
}

/// The capture host binds a cleared stand-in for a declared device picture,
/// so the frame rasterises, and rasterises again, with the red beneath the
/// picture showing through it.
#[kithara::test(timeout(Short))]
fn capture_host_draws_a_declared_device_picture_it_cannot_bind_as_cleared() {
    let declaration =
        ExternalImageDeclaration::new(ImageId::new("fixture/preview"), 4, 4, ImageAlphaType::Alpha)
            .expect("the producer must declare a nonempty preview");
    let kinds = CustomKinds::default().with(
        "fixture-covered",
        move || Covered {
            declaration: declaration.clone(),
        },
        |()| UiEvent::Window(WindowCommand::Minimize),
    );
    let (endpoints, resolver) = (MapEndpoints::default(), documents());
    let mut ui = Ui::new(
        Still,
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .kinds(&kinds)
            .build(),
        (32, 32),
        1.0,
    )
    .expect("the covered document must mount");
    ui.frame(Duration::from_millis(16));
    let frame = ui.render().expect("the covered widget must paint");
    ui.complete_frame();
    assert_eq!(frame.external_images().len(), 1);

    let mut offscreen =
        Offscreen::new(32, 32).expect("this lane runs where a graphics device exists");
    let mut rgba = Vec::new();
    for pass in 0..2 {
        offscreen
            .rasterise(&frame, 1.0, Color::from_rgba8(0, 0, 255, 255), &mut rgba)
            .expect("a frame that declares a device picture must rasterise");
        let centre = (16 * 32 + 16) * 4;
        assert_eq!(
            rgba.get(centre..centre + 4),
            Some([255_u8, 0, 0, 255].as_slice()),
            "pass {pass}: the red beneath the picture must show through it"
        );
    }
}
