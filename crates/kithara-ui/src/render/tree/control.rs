use iced::advanced::{layout::Layout, mouse};

use super::{
    geometry::Rendered,
    host::{tree_input_layout, tree_search_input_layout},
    mount::{Cx, ViewControl},
    read_scope, resolve,
    track_list::TrackListHost,
};
use crate::{
    atoms::{bar::context::Context, design::fader::rail_bounds},
    compile::CompiledUi,
    draw::Rect,
    engine::{Descriptor, Engine, Target},
    expand::{Binding, ControlSpec},
    ids::InternId,
    interact::{Hit, iced as iced_interact},
    mount,
    render::{HostedControlPlan, InputOwner, ReadValue, Reads, Skin},
};

pub(super) fn render_control<'a>(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
    owner: InputOwner,
) -> Rendered<'a> {
    let value = read.and_then(|binding| resolve(reads, binding, ui));
    let cx = Cx {
        owner,
        path: ui.resolve(path),
        reads,
        scope: read_scope(read, ui),
        skin,
        ui,
        value: value.as_ref(),
    };
    mount::controls!(spec, Mount { cx: &cx })
}

/// Asks whichever control the document named to mount itself here.
struct Mount<'cx, 'a, 'reads, 'value> {
    cx: &'cx Cx<'a, 'reads, 'value>,
}

impl<'a> Mount<'_, 'a, '_, '_> {
    fn apply<C: ViewControl>(self, control: &C) -> Rendered<'a> {
        control.view(self.cx)
    }
}

pub(super) struct HostedControl {
    plan: HostedControlPlan,
    track_list: Option<Box<TrackListHost>>,
}

impl HostedControl {
    pub(super) fn new(
        path: &str,
        spec: &ControlSpec,
        value: Option<ReadValue<'_>>,
        scope: &str,
        reads: &dyn Reads,
        ui: &CompiledUi,
        skin: &Skin,
    ) -> Option<Self> {
        HostedControlPlan::resolved(path, spec, value, scope, reads, ui, skin)
            .map(|plan| Self::mounted(plan, skin))
    }

    pub(super) fn mounted(plan: HostedControlPlan, skin: &Skin) -> Self {
        let track_list = match &plan {
            HostedControlPlan::TrackList(plan) => Some(Box::new(TrackListHost::new(
                &plan.path,
                plan.columns.clone(),
                plan.row_count,
                skin,
            ))),
            _ => None,
        };
        Self { plan, track_list }
    }

    delegate::delegate! {
        to self.plan {
            fn path(&self) -> &str;
        }
    }

    /// Narrows a control's rectangle to the part a pointer actually drives. A
    /// fader is one canvas holding a caption and a rail; only the rail answers.
    fn input_bounds(&self, bounds: Rect) -> Rect {
        match &self.plan {
            HostedControlPlan::Fader {
                style,
                labelled,
                metrics,
                ..
            } => rail_bounds(bounds, *style, *labelled, *metrics),
            HostedControlPlan::Picker { face, .. } => Context::placed(*face, bounds),
            _ => bounds,
        }
    }

    pub(super) fn picker(&self) -> Option<(&str, usize, f32)> {
        match &self.plan {
            HostedControlPlan::Picker {
                path,
                item_count,
                item_height,
                ..
            } => Some((path, *item_count, *item_height)),
            _ => None,
        }
    }
}

pub(super) fn append_control_targets<'a>(
    control: &'a HostedControl,
    layout: Layout<'_>,
    cursor: mouse::Cursor,
    engine: Option<&Engine>,
    targets: &mut Vec<Target<'a>>,
) {
    if let HostedControlPlan::Tree {
        path, search_path, ..
    } = &control.plan
    {
        if let Some(layout) = tree_search_input_layout(layout) {
            targets.push(Target::new(
                search_path,
                iced_interact::hit(layout.bounds(), cursor),
            ));
        }
        if let Some(layout) = tree_input_layout(layout) {
            targets.push(Target::new(
                path,
                iced_interact::hit(layout.bounds(), cursor),
            ));
        }
        return;
    }
    if let Some(track_list) = &control.track_list {
        track_list.append_targets(layout, cursor, engine, targets);
    } else {
        targets.push(Target::new(
            control.path(),
            Hit::new(
                cursor.position().map(Into::into),
                control.input_bounds(layout.bounds().into()),
            ),
        ));
    }
}

pub(super) fn append_control_descriptors(
    control: &HostedControl,
    descriptors: &mut Vec<Descriptor>,
) {
    descriptors.extend(control.plan.descriptors());
}
