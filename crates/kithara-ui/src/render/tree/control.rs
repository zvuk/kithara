use iced::advanced::{layout::Layout, mouse};

use super::{
    geometry::{Rendered, tree_input_layout, tree_search_input_layout},
    mount::{Cx, ViewControl},
    table::TableHost,
};
use crate::{
    atoms::{bar::context::Context, design::fader::rail_bounds},
    draw::{Rect, Transform},
    engine::{Descriptor, Engine, Target},
    expand::{Binding, ControlSpec},
    ids::InternId,
    interact::{Hit, iced as iced_interact},
    mount,
    render::{HostedControlPlan, InputOwner, ReadValue, Resolving, Skin, document::Ctx},
};

/// How the document placed one control: who owns its pointer, and the offset
/// every enclosing object folded into its box.
#[derive(Clone, Copy)]
pub(super) struct Placed {
    pub(super) owner: InputOwner,
    pub(super) transform: Transform,
}

pub(super) fn render_control<'a>(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ctx: Ctx<'a, '_>,
    skin: &'a Skin,
    placed: Placed,
) -> Rendered<'a> {
    let value = read.and_then(|binding| ctx.read(binding));
    let cx = Cx {
        owner: placed.owner,
        transform: placed.transform,
        path: ctx.ui.resolve(path),
        ctx,
        scope: ctx.scope(read),
        skin,
        value: value.as_ref(),
    };
    mount::controls!(spec, Mount { cx: &cx })
}

/// Asks whichever control the document named to mount itself here.
struct Mount<'cx, 'a, 'ctx, 'value> {
    cx: &'cx Cx<'a, 'ctx, 'value>,
}

impl<'a> Mount<'_, 'a, '_, '_> {
    fn apply<C: ViewControl>(self, control: &C) -> Rendered<'a> {
        control.view(self.cx)
    }
}

pub(super) struct HostedControl {
    plan: HostedControlPlan,
    table: Option<Box<TableHost>>,
}

impl HostedControl {
    pub(super) fn new(
        path: &str,
        spec: &ControlSpec,
        value: Option<ReadValue<'_>>,
        read: Option<&Binding>,
        scope: &str,
        cx: Resolving<'_>,
    ) -> Option<Self> {
        HostedControlPlan::resolved(path, spec, value, read, scope, cx)
            .map(|plan| Self::mounted(plan, cx.skin))
    }

    pub(super) fn mounted(plan: HostedControlPlan, skin: &Skin) -> Self {
        let table = match &plan {
            HostedControlPlan::Table(plan) => Some(Box::new(TableHost::new(
                &plan.path,
                plan.columns(),
                plan.row_count(),
                skin,
            ))),
            _ => None,
        };
        Self { plan, table }
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
                items,
                item_height,
                ..
            } => Some((path, items.len(), *item_height)),
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
    if let HostedControlPlan::Tree(plan) = &control.plan {
        if let Some(layout) = tree_search_input_layout(layout) {
            targets.push(Target::new(
                &plan.search_path,
                iced_interact::hit(layout.bounds(), cursor),
            ));
        }
        if let Some(layout) = tree_input_layout(layout) {
            targets.push(Target::new(
                &plan.path,
                iced_interact::hit(layout.bounds(), cursor),
            ));
        }
        return;
    }
    if let Some(table) = &control.table {
        table.append_targets(layout, cursor, engine, targets);
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
