use iced::{alignment::Horizontal, widget::Space};

use super::{
    geometry::Rendered,
    panel::{context_bar, table, tree, vis},
    window::{titlebar, window_controls},
};
use crate::{
    draw::Transform,
    hosts::controls::{Draws, Reading},
    iced::{
        controls::{Gesture, Paint},
        immediate::{Custom, MiniWave, Text},
        paint::PainterLength,
        shader,
        tree::Widget,
        window::WindowSurface,
    },
    module::TextAlign,
    mount,
    render::{InputOwner, ReadValue, Skin, document::Ctx},
};

/// How one built-in control becomes an element of the immediate-mode tree.
///
/// Every control answers for itself, so the host walks the document and hands
/// each one the same surroundings instead of keeping a table of what each
/// control is made of.
pub(crate) trait ViewControl {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a>;
}

/// What a control is handed when it mounts: the document it was read from, the
/// value behind it, and who owns the pointer over it.
pub(crate) struct Cx<'a, 'ctx, 'value> {
    pub(super) skin: &'a Skin,
    pub(super) path: &'a str,
    pub(super) scope: &'a str,
    pub(super) ctx: Ctx<'a, 'ctx>,
    pub(super) owner: InputOwner,
    pub(super) value: Option<&'value ReadValue<'ctx>>,
    /// Every enclosing object's pose, folded into the box this control paints
    /// into. Identity for a control no object wraps.
    pub(super) transform: Transform,
}

impl ViewControl for mount::Drag {
    fn view<'a>(&self, _cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(WindowSurface::drag().view())
    }
}

impl ViewControl for mount::window::title_bar::host::TitleBar {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(titlebar(self.label, cx.ctx.ui, cx.skin))
    }
}

impl ViewControl for mount::Controls {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(window_controls(self.style, cx.skin))
    }
}

impl ViewControl for mount::label::text::host::Text<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::new(
            Text::builder()
                .style(self.style)
                .maybe_value(cx.value)
                .maybe_label(self.label.map(|id| cx.ctx.ui.resolve(id)))
                .maybe_color(cx.ctx.color_role(self.color))
                .maybe_active_color(self.active_color)
                .active(cx.ctx.flag(self.active))
                .maybe_font(self.font)
                .maybe_weight(self.weight)
                .skin(cx.skin)
                .build()
                .view(),
            horizontal(self.align),
        )
    }
}

impl ViewControl for mount::label::glyph::host::Glyph<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

/// The wave draws the same on both hosts; only the immediate host recognises
/// the loop and zoom gestures itself, and only where the document left it the
/// pointer. Reconciling that with the retained host's plan is the gesture
/// census, not this.
impl ViewControl for mount::deck::wave::host::Wave<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        if cx.owner == InputOwner::Engine {
            return painted(self, cx);
        }
        let Some(data) = self.data(reading(cx)) else {
            return Rendered::leading(Space::new().into());
        };
        Rendered::leading(
            MiniWave::builder()
                .path(cx.path)
                .style(self.style)
                .data(data)
                .skin(cx.skin)
                .build()
                .view(),
        )
    }
}

impl ViewControl for mount::Vis {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(vis(cx.value, cx.ctx))
    }
}

impl ViewControl for mount::panel::shader::host::Shader<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(shader::view(self.spec, cx.path, cx.ctx))
    }
}

impl ViewControl for mount::panel::custom::host::Custom {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(Custom::new(cx.ctx.ui.resolve(self.kind), cx.ctx.kinds, cx.skin).into())
    }
}

impl ViewControl for mount::panel::table::host::Table<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(table(cx, self))
    }
}

impl ViewControl for mount::Search {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        let query = match cx.value {
            Some(ReadValue::Text(query)) => *query,
            _ => "",
        };
        Rendered::leading(crate::iced::immediate::search_bar(
            cx.path, query, cx.skin, cx.owner,
        ))
    }
}

impl ViewControl for mount::panel::tree::host::Tree<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(tree(cx.path, self, cx.value, cx.ctx, cx.skin, cx.owner))
    }
}

impl ViewControl for mount::panel::context_bar::host::ContextBar<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        let Some(data) = self.data(reading(cx)) else {
            return Rendered::leading(Space::new().into());
        };
        Rendered::leading(context_bar(
            cx,
            (self.scope_items, self.scope),
            self.painter(cx.skin),
            data,
        ))
    }
}

/// Mounts a control that draws itself, adding nothing to the picture.
pub(crate) fn painted<'a, Control>(control: &Control, cx: &Cx<'a, '_, '_>) -> Rendered<'a>
where
    Control: Draws,
    Control::Painter: PainterLength + 'static,
{
    let Some(data) = control.data(reading(cx)) else {
        return Rendered::leading(Space::new().into());
    };
    let grip = control.grip(cx.skin, &data);
    let index_event = control.index_event();
    let paint = Paint::pooled(
        control.painter(cx.skin),
        data,
        cx.skin,
        &cx.ctx.ui.draw_buffers,
    )
    .posed(cx.transform);
    let element = if cx.owner == InputOwner::Leaf {
        Gesture::with_grip(cx.path, paint, grip, index_event)
            .map_or_else(Paint::view, Gesture::view)
    } else {
        paint.view()
    };
    Rendered::leading(element)
}

/// What a control is handed when it decides what to draw, from what this host
/// was handed when it mounted one.
fn reading<'a, 'ctx, 'value>(cx: &'a Cx<'a, 'ctx, 'value>) -> Reading<'a>
where
    'ctx: 'a,
    'value: 'a,
{
    Reading {
        ctx: cx.ctx,
        scope: cx.scope,
        skin: cx.skin,
        value: cx.value,
    }
}

fn horizontal(align: TextAlign) -> Horizontal {
    match align {
        TextAlign::Start => Horizontal::Left,
        TextAlign::Center => Horizontal::Center,
        TextAlign::End => Horizontal::Right,
    }
}

impl ViewControl for mount::Preset {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::deck::summary::host::Summary {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::panel::lottie::host::Lottie<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::panel::sprite::host::Sprite {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}
