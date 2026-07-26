use iced::{
    Element, Length,
    widget::{Column, Row, Space, Stack},
};

use super::{node::render_compiled, read::resolve};
use crate::{
    compile::{CompiledNode, CompiledUi},
    ids::InternId,
    module::WindowControlsStyle,
    render::{ReadValue, Reads, Skin, UiEvent, WindowEdge},
    widgets::{
        Widget,
        drag_ghost::DragGhost,
        window::{TitleBar, WindowControls, WindowSurface},
    },
};

pub fn render<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let content = render_compiled(node, ui, reads, skin);
    let content = if ui.resize_edges {
        framed_by_resize_edges(content, skin)
    } else {
        content
    };
    if ui.dragged.is_none() {
        return content;
    }
    with_drag_ghost(content, dragged_label(ui, reads), skin)
}

/// Always laid out, even empty-handed: a layer that came and went would reset the gesture.
fn with_drag_ghost<'a>(
    content: Element<'a, UiEvent>,
    label: Option<String>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    Stack::with_children([content, DragGhost::new(label, skin).view()])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn dragged_label(ui: &CompiledUi, reads: &dyn Reads) -> Option<String> {
    let binding = ui.dragged.as_ref()?;
    match resolve(reads, binding, ui)? {
        ReadValue::Text(label) if !label.is_empty() => Some(label.to_owned()),
        _ => None,
    }
}

/// The zones sit above the content, not beside it, so the layout keeps the whole window.
fn framed_by_resize_edges<'a>(content: Element<'a, UiEvent>, skin: &Skin) -> Element<'a, UiEvent> {
    let thickness = Length::Fixed(skin.window.resize_edge);
    let corner = |edge| WindowSurface::resize(edge, thickness, thickness).view();
    let side = |edge, width, height| WindowSurface::resize(edge, width, height).view();
    let edges = Column::with_children([
        Row::with_children([
            corner(WindowEdge::NorthWest),
            side(WindowEdge::North, Length::Fill, thickness),
            corner(WindowEdge::NorthEast),
        ])
        .height(thickness)
        .into(),
        Row::with_children([
            side(WindowEdge::West, thickness, Length::Fill),
            Space::new().width(Length::Fill).height(Length::Fill).into(),
            side(WindowEdge::East, thickness, Length::Fill),
        ])
        .height(Length::Fill)
        .into(),
        Row::with_children([
            corner(WindowEdge::SouthWest),
            side(WindowEdge::South, Length::Fill, thickness),
            corner(WindowEdge::SouthEast),
        ])
        .height(thickness)
        .into(),
    ])
    .width(Length::Fill)
    .height(Length::Fill);
    Stack::with_children([content, edges.into()])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

pub(super) fn titlebar<'a>(
    label: InternId,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    TitleBar::builder()
        .label(ui.resolve(label))
        .skin(skin)
        .build()
        .view()
}

pub(super) fn window_controls(
    style: WindowControlsStyle,
    skin: &Skin,
) -> Element<'static, UiEvent> {
    WindowControls::builder()
        .style(style)
        .skin(skin)
        .build()
        .view()
}
