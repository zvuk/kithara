use iced::{alignment::Horizontal, widget::Space};

use super::{
    geometry::Rendered,
    panel::{context_bar, track_list, tree, vis},
    read_flag,
    window::{titlebar, window_controls},
};
use crate::{
    compile::CompiledUi,
    module::TextAlign,
    mount,
    render::{
        InputOwner, ReadValue, Reads, Skin,
        controls::{Draws, Gesture, Grip, Paint, Reading},
    },
    widgets::{
        Widget, global_bar::PresetSelector, text::Text, wave::mini::MiniWave, window::WindowSurface,
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
pub(super) struct Cx<'a, 'reads, 'value> {
    pub(super) owner: InputOwner,
    pub(super) path: &'a str,
    pub(super) reads: &'reads dyn Reads,
    pub(super) scope: &'a str,
    pub(super) skin: &'a Skin,
    pub(super) ui: &'a CompiledUi,
    pub(super) value: Option<&'value ReadValue<'reads>>,
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
        Rendered::leading(
            PresetSelector::builder()
                .reads(cx.reads)
                .skin(cx.skin)
                .build()
                .view(),
        )
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
        Rendered::leading(titlebar(self.label, cx.ui, cx.skin))
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
                .maybe_label(self.label.map(|id| cx.ui.resolve(id)))
                .maybe_color(self.color)
                .maybe_active_color(self.active_color)
                .active(read_flag(self.active, cx.reads, cx.ui))
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
        Rendered::leading(vis(cx.value, cx.reads))
    }
}

impl ViewControl for mount::TrackList<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(track_list(
            cx.path,
            (self.columns, self.columns_state),
            cx.value,
            cx.ui,
            cx.reads,
            cx.skin,
            cx.owner,
        ))
    }
}

impl ViewControl for mount::Tree<'_> {
    fn view<'a>(&self, cx: &Cx<'a, '_, '_>) -> Rendered<'a> {
        Rendered::leading(tree(
            cx.path, self.query, cx.value, cx.ui, cx.reads, cx.skin, cx.owner,
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

impl ViewControl for mount::StatusDot {
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
    let paint = Paint::new(control.painter(cx.skin), data, cx.skin);
    Rendered::leading(match (cx.owner, grip) {
        (InputOwner::Leaf, Grip::Press) => Gesture::press(cx.path, paint).view(),
        (InputOwner::Leaf, Grip::Command(event)) => Gesture::command(cx.path, paint, event).view(),
        (InputOwner::Leaf, Grip::Drag(drag)) => Gesture::drag(cx.path, paint, drag).view(),
        (InputOwner::Leaf, Grip::Index { count }) => Gesture::index(cx.path, paint, count).view(),
        (InputOwner::Engine, _) | (_, Grip::None) => paint.view(),
    })
}

/// What a control is handed when it decides what to draw, from what this host
/// was handed when it mounted one.
fn reading<'a, 'reads, 'value>(cx: &'a Cx<'a, 'reads, 'value>) -> Reading<'a>
where
    'reads: 'a,
    'value: 'a,
{
    Reading {
        reads: cx.reads,
        scope: cx.scope,
        ui: cx.ui,
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
