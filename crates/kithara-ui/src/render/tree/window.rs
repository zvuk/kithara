use iced::Element;

use super::node::IcedHost;
use crate::{
    compile::{CompiledNode, CompiledUi},
    ids::InternId,
    module::WindowControlsStyle,
    render::{
        Reads, Skin, TitleBar, UiEvent, Widget, WindowControls, document,
        document::{Clock, Ctx},
    },
};

/// Draws one frame of the document.
///
/// `clock` is this host's own reading of time, so a caller that drives it
/// reproduces a frame exactly rather than waiting for a wall clock.
pub fn render<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
    clock: Clock,
) -> Element<'a, UiEvent> {
    let ctx = Ctx::new(ui, reads, skin.document(), clock);
    document::render(node, ctx, IcedHost::new(ctx, skin))
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
