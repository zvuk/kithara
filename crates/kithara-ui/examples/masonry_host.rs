use std::error::Error;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_ui::{
    builtin,
    compile::compile,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    ids::{EndpointId, SourceUri},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry},
    render::{
        ReadValue, Reads, Skin, document,
        document::{Clock, Ctx},
        masonry::{
            CustomWidget, MasonryHost, MasonryRoot, MasonryState, Repaint, Size2, SizeLimits,
            TextMeasurer,
        },
    },
    shaping::FontPolicy,
    source::{MemResolver, UiConfig},
};
use masonry::{
    app::{RenderRootOptions, WindowSizePolicy},
    core::WindowEvent,
    dpi::PhysicalSize,
    theme::default_property_set,
};

struct EmptyRegistry;

impl EndpointRegistry for EmptyRegistry {
    fn endpoint(&self, _category: EndpointCategory, _id: &EndpointId) -> Option<&EndpointDesc> {
        None
    }
}

struct EmptyReads;

impl Reads for EmptyReads {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

struct LetterboxedBadge {
    role: kithara_ui::skin::TextRoleSkin,
}

impl CustomWidget for LetterboxedBadge {
    type Action = ();

    fn measure(&mut self, text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        let label = text.measure("MASONRY", self.role, None);
        Size2::new(label.w.max(160.0), 90.0)
    }

    fn paint(&mut self, list: &mut DrawListBuilder, text: &mut TextMeasurer<'_>, bounds: Rect) {
        let authored = Size2::new(160.0, 90.0);
        let scale = (bounds.w / authored.w).min(bounds.h / authored.h);
        let fitted = Size2::new(authored.w * scale, authored.h * scale);
        let fitted = Rect {
            x: bounds.x + (bounds.w - fitted.w) / 2.0,
            y: bounds.y + (bounds.h - fitted.h) / 2.0,
            w: fitted.w,
            h: fitted.h,
        };
        list.fill_rounded_rect(
            fitted,
            8.0,
            Rgba {
                r: 0.15,
                g: 0.18,
                b: 0.32,
                a: 1.0,
            },
        );
        let run = text.shape("MASONRY", self.role, Some(fitted.w));
        list.text(
            &run,
            "MASONRY",
            Transform::translate(Pt {
                x: fitted.x + (fitted.w - run.width()) / 2.0,
                y: fitted.y + (fitted.h - run.height()) / 2.0,
            }),
            Rgba {
                r: 0.9,
                g: 0.78,
                b: 0.35,
                a: 1.0,
            },
        );
    }

    fn repaint(&self) -> Repaint {
        Repaint::Continuous
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "masonry.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "masonry",
            root: Module(
                instance: "demo",
                source: "masonry.kmodule.ron",
                size: (w: Fill, h: Fill),
            ))"#,
    );
    resolver.insert(
        "masonry.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "masonry", chrome: Plain,
            root: Spacer(id: "custom", size: Some((w: Fill, h: Fill))))"#,
    );
    let ui = compile(
        "masonry.klayout.ron",
        &resolver,
        &EmptyRegistry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )?;
    let skin = Skin::resolve_with_font_policy(
        builtin::skin_doc().clone(),
        builtin::text_doc(),
        &SourceUri("example:masonry".to_owned()),
        FontPolicy::Embedded,
    )?;
    let reads = EmptyReads;
    let state = MasonryState::default();
    let ctx = Ctx::new(&ui, &reads, builtin::skin_doc(), Clock::default());
    let host = MasonryHost::new(ctx, &skin).with_state(state).with_custom(
        "demo/custom",
        LetterboxedBadge {
            role: skin.text.section,
        },
        |()| kithara_ui::render::UiEvent::OpenSettings,
    );
    let root = document::render(&ui.root, ctx, host);
    let mut root = MasonryRoot::new(
        root,
        RenderRootOptions {
            default_properties: Arc::new(default_property_set()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(640, 360),
            scale_factor: 1.0,
            test_font: None,
        },
    )?;
    root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(16)))?;
    let _scene = root.redraw()?;
    let _events = root.take_actions();
    let _signals = root.take_platform_signals();
    Ok(())
}
