use iced::advanced::layout::Layout;
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_tiny_skia::Renderer as TinySkiaRenderer;

use crate::{
    draw::Rect,
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

pub(super) struct Endpoints {
    hidden: EndpointDesc,
    open: EndpointDesc,
    press: EndpointDesc,
    rate: EndpointDesc,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            hidden: EndpointDesc::new(ValueKind::Bool),
            rate: EndpointDesc::new(ValueKind::Scalar),
            open: EndpointDesc::new(ValueKind::Bool),
            press: EndpointDesc::new(ValueKind::Trigger),
        }
    }
}

impl EndpointRegistry for Endpoints {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        match (category, id.0.as_str()) {
            (EndpointCategory::Model, "fixture.hidden") => Some(&self.hidden),
            (EndpointCategory::Model, "fixture.menu") => Some(&self.open),
            (EndpointCategory::Command, "fixture.toggle") => Some(&self.press),
            (EndpointCategory::Parameter, "fixture.rate") => Some(&self.rate),
            _ => None,
        }
    }
}

/// How the rect corpus compares the two hosts: one lays out in whole pixels and
/// the other in fractions of one, so both edges are snapped before they meet.
pub(in crate::render) fn snapped(rect: Rect) -> [f32; 4] {
    let x = rect.x.round();
    let y = rect.y.round();
    [
        x,
        y,
        (rect.x + rect.w).round() - x,
        (rect.y + rect.h).round() - y,
    ]
}

pub(in crate::render) fn renderer() -> iced::Renderer {
    FallbackRenderer::Secondary(TinySkiaRenderer::new(
        crate::render::fonts::SANS,
        iced::Pixels(14.0),
    ))
}

pub(in crate::render) fn collect_rows(layout: Layout<'_>, rows: &mut Vec<Rect>) {
    let mut children = layout.children().peekable();
    if children.peek().is_none() {
        let bounds = layout.bounds();
        rows.push(Rect {
            x: bounds.x,
            y: bounds.y,
            w: bounds.width,
            h: bounds.height,
        });
        return;
    }
    for child in children {
        collect_rows(child, rows);
    }
}
