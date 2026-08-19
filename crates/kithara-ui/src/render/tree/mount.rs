use iced::{alignment::Horizontal, widget::Space};

use super::{
    geometry::Rendered,
    panel::{context_bar, table, tree, vis},
    window::{titlebar, window_controls},
};
use crate::{
    draw::Transform,
    module::TextAlign,
    mount,
    render::{
        InputOwner, MiniWave, ReadValue, Skin, Text, Widget, WindowSurface,
        controls::{Draws, Gesture, Paint, Reading},
        document::Ctx,
        shader,
    },
};

/// How one built-in control becomes an element of the immediate-mode tree.
///
/// Every control answers for itself, so the host walks the document and hands
/// each one the same surroundings instead of keeping a table of what each
/// control is made of.
pub(super) trait ViewControl {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a>;
}

/// What a control is handed when it mounts: the document it was read from, the
/// value behind it, and who owns the pointer over it.
pub(super) struct Cx<'a, 'ctx, 'value> {
    pub(super) owner: InputOwner,
    pub(super) path: &'a str,
    pub(super) ctx: Ctx<'a, 'ctx>,
    pub(super) scope: &'a str,
    pub(super) skin: &'a Skin,
    /// Every enclosing object's pose, folded into the box this control paints
    /// into. Identity for a control no object wraps.
    pub(super) transform: Transform,
    pub(super) value: Option<&'value ReadValue<'ctx>>,
}

impl ViewControl for mount::Summary {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Brand {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Spacer {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Divider {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Preset {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Settings {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Drag {
    fn view<'a>(&self, _cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(WindowSurface::drag().view())
    }
}

impl ViewControl for mount::TitleBar {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(titlebar(self.label, cx.ctx.ui, cx.skin))
    }
}

impl ViewControl for mount::Controls {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(window_controls(self.style, cx.skin))
    }
}

impl ViewControl for mount::Text<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::new(
            Text::builder()
                .style(self.style)
                .maybe_value(cx.value)
                .maybe_label(self.label.map(|id| cx.ctx.ui.resolve(id)))
                .maybe_color(self.color)
                .maybe_active_color(self.active_color)
                .active(cx.ctx.flag(self.active))
                .skin(cx.skin)
                .build()
                .view(),
            horizontal(self.align),
        )
    }
}

impl ViewControl for mount::Glyph<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::NavItem {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Tab {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Button {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Bpm {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Time {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Telemetry {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Crossfader {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Fader {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

/// The wave draws the same on both hosts; only the immediate host recognises
/// the loop and zoom gestures itself, and only where the document left it the
/// pointer. Reconciling that with the retained host's plan is the gesture
/// census, not this.
impl ViewControl for mount::Wave<'_> {
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

impl ViewControl for mount::Shader<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(shader::view(self.spec, cx.path, cx.ctx))
    }
}

impl ViewControl for mount::Lottie {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Sprite {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::PortalMap {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Range {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Table<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(table(cx, (self.columns, self.columns_state)))
    }
}

impl ViewControl for mount::Tree<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(tree(
            cx.path, self.query, cx.value, cx.ctx, cx.skin, cx.owner,
        ))
    }
}

impl ViewControl for mount::ContextBar<'_> {
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

impl ViewControl for mount::Toggle {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Checkbox {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Segmented<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Select {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::StatusDot<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Swatch {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Cell {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Readout {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Chip {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Knob {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::Meter {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::VuStereo {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

impl ViewControl for mount::VuVertical {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        painted(self, cx)
    }
}

/// Mounts a control that draws itself, adding nothing to the picture.
fn painted<'a, Control>(control: &Control, cx: &Cx<'a, '_, '_>) -> Rendered<'a>
where
    Control: Draws,
    Control::Painter: 'static,
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
        cx.ctx.ui.draw_pools(),
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
