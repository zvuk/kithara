use super::{
    Binding, ControlSpec,
    binding_subst::{intern_optional_binding, intern_optional_text, intern_text, intern_texts},
    machine::Context,
    site::ExtraBindings,
};
use crate::{
    error::UiDocError,
    ids::{InternId, Interner},
    module::{BindingRef, ControlNode, Tone, TrackColumn, WaveStyle},
};

fn context_bar_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    scope_items: &[String],
    scope: Option<&BindingRef>,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::ContextBar {
        scope_items: intern_texts(context, interner, scope_items, path, &context.origin)?,
        scope: intern_optional_binding(interner, scope, &context.origin)?,
    })
}

struct StatusDotFields<'a> {
    label: &'a str,
    dot_size: Option<f32>,
    tone: Tone,
    active_tone: Option<Tone>,
}

fn status_dot_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    fields: &StatusDotFields<'_>,
    extra: &ExtraBindings,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::StatusDot {
        label: intern_text(context, interner, fields.label, path, &context.origin)?,
        dot_size: fields.dot_size,
        tone: fields.tone,
        active_tone: fields.active_tone,
        active: optional_binding(context, interner, extra.active.as_ref())?,
    })
}

fn title_bar_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    label: &str,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::TitleBar {
        label: intern_text(context, interner, label, path, &context.origin)?,
    })
}

/// A container node has no spec of its own; the caller keeps walking it.
pub(super) fn control_spec(
    context: &Context<'_>,
    control: &ControlNode,
    extra: &ExtraBindings,
    path: &str,
    interner: &mut Interner,
) -> Result<Option<ControlSpec>, UiDocError> {
    let spec = match control {
        ControlNode::Glyph {
            icon,
            active_icon,
            style,
            color,
            active_color,
            ..
        } => ControlSpec::Glyph {
            icon: context.param(icon, path)?,
            active_icon: context.optional_param(active_icon.as_ref(), path)?,
            style: *style,
            color: context.optional_param(color.as_ref(), path)?,
            active_color: context.optional_param(active_color.as_ref(), path)?,
            active: optional_binding(context, interner, extra.active.as_ref())?,
        },
        ControlNode::TitleBar { label, .. } => title_bar_spec(context, interner, label, path)?,
        ControlNode::Text {
            style,
            label,
            align,
            color,
            active_color,
            ..
        } => ControlSpec::Text {
            style: *style,
            label: optional_text(context, interner, label.as_deref(), path)?,
            color: *color,
            active_color: *active_color,
            active: optional_binding(context, interner, extra.active.as_ref())?,
            align: *align,
        },
        ControlNode::NavItem { label, icon, .. } => ControlSpec::NavItem {
            label: intern_text(context, interner, label, path, &context.origin)?,
            icon: context.param(icon, path)?,
        },
        ControlNode::TabLarge { label, .. } => ControlSpec::TabLarge {
            label: intern_text(context, interner, label, path, &context.origin)?,
        },
        ControlNode::Button {
            label,
            icon,
            active_label,
            style,
            frame,
            ..
        } => ControlSpec::Button {
            label: intern_text(context, interner, label, path, &context.origin)?,
            icon: context.optional_param(icon.as_ref(), path)?,
            active_label: optional_text(context, interner, active_label.as_deref(), path)?,
            style: *style,
            frame: *frame,
        },
        ControlNode::Bpm { placeholder, .. } => ControlSpec::Bpm {
            placeholder: optional_text(context, interner, placeholder.as_deref(), path)?,
        },
        ControlNode::Fader { style, label, .. } => ControlSpec::Fader {
            style: *style,
            label: optional_text(context, interner, label.as_deref(), path)?,
        },
        ControlNode::Wave { style, badge, .. } => wave_spec(
            context,
            interner,
            *style,
            badge.as_deref(),
            extra.zoom.as_ref(),
            path,
        )?,
        ControlNode::TrackList { columns, .. } => {
            track_list_spec(context, interner, columns, extra)?
        }
        ControlNode::Tree { .. } => ControlSpec::Tree {
            query: optional_binding(context, interner, extra.query.as_ref())?,
        },
        ControlNode::ContextBar { scope_items, .. } => {
            context_bar_spec(context, interner, scope_items, extra.scope.as_ref(), path)?
        }
        ControlNode::Segmented { items, .. } => ControlSpec::Segmented {
            items: intern_texts(context, interner, items, path, &context.origin)?,
        },
        ControlNode::Select { label, .. } => ControlSpec::Select {
            label: intern_text(context, interner, label, path, &context.origin)?,
        },
        ControlNode::StatusDot {
            label,
            dot_size,
            tone,
            active_tone,
            ..
        } => status_dot_spec(
            context,
            interner,
            &StatusDotFields {
                label,
                dot_size: *dot_size,
                tone: *tone,
                active_tone: *active_tone,
            },
            extra,
            path,
        )?,
        ControlNode::Swatch { role, label, .. } => ControlSpec::Swatch {
            role: *role,
            label: intern_text(context, interner, label, path, &context.origin)?,
        },
        ControlNode::Cell {
            label, highlighted, ..
        } => ControlSpec::Cell {
            label: optional_text(context, interner, label.as_deref(), path)?,
            highlighted: *highlighted,
        },
        ControlNode::Readout {
            label,
            tone,
            framed,
            ..
        } => ControlSpec::Readout {
            label: optional_text(context, interner, label.as_deref(), path)?,
            tone: *tone,
            framed: *framed,
        },
        ControlNode::Chip { label, style, .. } => ControlSpec::Chip {
            label: intern_text(context, interner, label, path, &context.origin)?,
            style: *style,
        },
        ControlNode::Knob { label, .. } => ControlSpec::Knob {
            label: optional_text(context, interner, label.as_deref(), path)?,
        },
        _ => return Ok(fixed_control_spec(control)),
    };
    Ok(Some(spec))
}

/// Specs for controls that need no context; the match stays exhaustive so a new
/// `ControlNode` has to be classified here rather than falling through silently.
fn fixed_control_spec(control: &ControlNode) -> Option<ControlSpec> {
    match control {
        ControlNode::DeckSummary { style, .. } => Some(ControlSpec::DeckSummary { style: *style }),
        ControlNode::Brand { .. } => Some(ControlSpec::Brand),
        ControlNode::Spacer { .. } => Some(ControlSpec::Spacer),
        ControlNode::Divider { .. } => Some(ControlSpec::Divider),
        ControlNode::PresetSelector { .. } => Some(ControlSpec::PresetSelector),
        ControlNode::SettingsButton { .. } => Some(ControlSpec::SettingsButton),
        ControlNode::WindowDrag { .. } => Some(ControlSpec::WindowDrag),
        ControlNode::WindowControls { style, .. } => {
            Some(ControlSpec::WindowControls { style: *style })
        }
        ControlNode::Time { .. } => Some(ControlSpec::Time),
        ControlNode::Scalar { format, framed, .. } => Some(ControlSpec::Scalar {
            format: *format,
            framed: *framed,
        }),
        ControlNode::Crossfader { ticks, .. } => Some(ControlSpec::Crossfader { ticks: *ticks }),
        ControlNode::Vis { .. } => Some(ControlSpec::Vis),
        ControlNode::PortalMap { .. } => Some(ControlSpec::PortalMap),
        ControlNode::Range { .. } => Some(ControlSpec::Range),
        ControlNode::Toggle { .. } => Some(ControlSpec::Toggle),
        ControlNode::Checkbox { .. } => Some(ControlSpec::Checkbox),
        ControlNode::Meter { .. } => Some(ControlSpec::Meter),
        ControlNode::VuStereo { .. } => Some(ControlSpec::VuStereo),
        ControlNode::VuVertical { ticks, .. } => Some(ControlSpec::VuVertical { ticks: *ticks }),
        ControlNode::Row { .. }
        | ControlNode::Adaptive { .. }
        | ControlNode::Column { .. }
        | ControlNode::Scroll { .. }
        | ControlNode::Include { .. }
        | ControlNode::Optional { .. }
        | ControlNode::Popover { .. }
        | ControlNode::Reveal { .. }
        | ControlNode::Pressable { .. }
        | ControlNode::Slot { .. }
        | ControlNode::Glyph { .. }
        | ControlNode::TitleBar { .. }
        | ControlNode::Text { .. }
        | ControlNode::NavItem { .. }
        | ControlNode::TabLarge { .. }
        | ControlNode::Button { .. }
        | ControlNode::Bpm { .. }
        | ControlNode::Fader { .. }
        | ControlNode::Wave { .. }
        | ControlNode::TrackList { .. }
        | ControlNode::Tree { .. }
        | ControlNode::ContextBar { .. }
        | ControlNode::Segmented { .. }
        | ControlNode::Select { .. }
        | ControlNode::StatusDot { .. }
        | ControlNode::Swatch { .. }
        | ControlNode::Cell { .. }
        | ControlNode::Readout { .. }
        | ControlNode::Chip { .. }
        | ControlNode::Knob { .. } => None,
    }
}

fn optional_text(
    context: &Context<'_>,
    interner: &mut Interner,
    label: Option<&str>,
    path: &str,
) -> Result<Option<InternId>, UiDocError> {
    intern_optional_text(context, interner, label, path, &context.origin)
}

fn optional_binding(
    context: &Context<'_>,
    interner: &mut Interner,
    binding: Option<&BindingRef>,
) -> Result<Option<Binding>, UiDocError> {
    intern_optional_binding(interner, binding, &context.origin)
}

fn wave_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    style: WaveStyle,
    badge: Option<&str>,
    zoom: Option<&BindingRef>,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Wave {
        style,
        badge: intern_optional_text(context, interner, badge, path, &context.origin)?,
        zoom: intern_optional_binding(interner, zoom, &context.origin)?,
    })
}

fn track_list_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    columns: &[TrackColumn],
    extra: &ExtraBindings,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::TrackList {
        columns: columns.to_vec(),
        columns_state: intern_optional_binding(
            interner,
            extra.columns_state.as_ref(),
            &context.origin,
        )?,
    })
}
