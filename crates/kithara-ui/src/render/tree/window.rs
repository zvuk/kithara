use iced::Element;

use super::node::IcedHost;
use crate::{
    compile::{CompiledNode, CompiledUi},
    ids::InternId,
    module::WindowControlsStyle,
    render::{Reads, Skin, UiEvent, document},
    widgets::{
        Widget,
        window::{TitleBar, WindowControls},
    },
};

pub fn render<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    document::render(
        node,
        ui,
        reads,
        skin.document(),
        IcedHost::new(ui, reads, skin),
    )
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
