use super::{
    Binding, ColorSpec, ControlSpec,
    binding_subst::{
        intern_binding, intern_optional_binding, intern_optional_text, intern_text, intern_texts,
        resolve_text_key,
    },
    machine::{Context, Expander},
    site::ExtraBindings,
};
use crate::{
    error::UiDocError,
    ids::{InternId, Interner, SourceUri},
    module::{BindingRef, ColorSource, ControlNode, TableColumn, TableFrame, Tone, WaveStyle},
    shader::{self, ShaderUniform},
    skin::ColorRole,
    validate,
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
    active_tone: Option<Tone>,
    dot_size: Option<f32>,
    tone: Tone,
}

fn status_dot_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    fields: &StatusDotFields<'_>,
    extra: &ExtraBindings,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::StatusDot {
        label: intern_text(
            context,
            machine.interner,
            fields.label,
            path,
            &context.origin,
        )?,
        dot_size: fields.dot_size,
        tone: fields.tone,
        active_tone: fields.active_tone,
        active: optional_binding(context, machine, extra.active.as_ref())?,
    })
}

fn shader_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    source: &str,
    extra: &ExtraBindings,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    let loaded =
        context
            .set
            .shader(&context.origin, source)
            .ok_or_else(|| UiDocError::NotFound {
                origin: context.origin.clone(),
                rel: source.to_owned(),
            })?;
    let fields = extra
        .uniforms
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<String>>();
    let uniforms = extra
        .uniforms
        .iter()
        .map(|(name, binding)| {
            Ok(ShaderUniform {
                kind: validate::shader_uniform_kind(
                    name,
                    binding,
                    path,
                    &context.origin,
                    machine.endpoints,
                )?,
                name: machine.interner.intern(name, &context.origin)?,
                read: intern_binding(machine.interner, binding, &context.origin)?,
            })
        })
        .collect::<Result<Vec<_>, UiDocError>>()?;
    Ok(ControlSpec::Shader(shader::compile(
        machine.shaders,
        &loaded.text,
        &loaded.uri,
        path,
        uniforms,
        &fields,
    )?))
}

fn title_bar_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    label: &str,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::TitleBar {
        label: intern_text(context, machine.interner, label, path, &context.origin)?,
    })
}

/// A container node has no spec of its own; the caller keeps walking it.
pub(super) fn control_spec(
    context: &Context<'_>,
    control: &ControlNode,
    extra: &ExtraBindings,
    path: &str,
    machine: &mut Expander<'_, '_>,
) -> Result<Option<ControlSpec>, UiDocError> {
    let spec = match control {
        ControlNode::Glyph { .. } => glyph_spec(context, machine, control, extra, path),
        ControlNode::Shader { source, .. } => shader_spec(context, machine, source, extra, path),
        ControlNode::Custom { kind, .. } => custom_spec(context, machine.interner, kind),
        ControlNode::Lottie { .. } => lottie_spec(context, machine, control, extra, path),
        ControlNode::Sprite { sheet, seconds, .. } => {
            sprite_spec(context, machine, sheet, *seconds, path)
        }
        ControlNode::TitleBar { label, .. } => title_bar_spec(context, machine, label, path),
        ControlNode::Text { .. } => text_spec(context, machine, control, extra, path),
        ControlNode::Wave { style, badge, .. } => wave_spec(
            context,
            machine,
            *style,
            badge.as_deref(),
            extra.zoom.as_ref(),
            path,
        ),
        ControlNode::Table { .. } => table_spec(context, machine, control, extra, path),
        ControlNode::Search { .. } => Ok(ControlSpec::Search),
        ControlNode::Tree { .. } => tree_spec(context, machine, extra),
        ControlNode::ContextBar { scope_items, .. } => context_bar_spec(
            context,
            machine.interner,
            scope_items,
            extra.scope.as_ref(),
            path,
        ),
        ControlNode::Segmented { items, .. } => {
            segmented_spec(context, machine.interner, items, path)
        }
        ControlNode::Select { label, .. } => select_spec(context, machine.interner, label, path),
        ControlNode::StatusDot {
            label,
            dot_size,
            tone,
            active_tone,
            ..
        } => status_dot_spec(
            context,
            machine,
            &StatusDotFields {
                label,
                dot_size: *dot_size,
                tone: *tone,
                active_tone: *active_tone,
            },
            extra,
            path,
        ),
        ControlNode::Swatch { role, label, .. } => {
            swatch_spec(context, machine.interner, *role, label, path)
        }
        _ => return caption_spec(context, control, path, machine),
    };
    spec.map(Some)
}

fn glyph_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    control: &ControlNode,
    extra: &ExtraBindings,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    let ControlNode::Glyph {
        icon,
        active_icon,
        style,
        active_color,
        ..
    } = control
    else {
        unreachable!("glyph_spec is called only for a glyph")
    };
    Ok(ControlSpec::Glyph {
        icon: context.param(icon, path)?,
        active_icon: context.optional_param(active_icon.as_ref(), path)?,
        style: *style,
        color: intern_optional_color(machine.interner, extra.color.as_ref(), &context.origin)?,
        active_color: context.optional_param(active_color.as_ref(), path)?,
        active: optional_binding(context, machine, extra.active.as_ref())?,
    })
}

fn custom_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    kind: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Custom {
        kind: interner.intern(kind, &context.origin)?,
    })
}

fn sprite_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    sheet: &str,
    seconds: f32,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Sprite {
        sheet: intern_text(context, machine.interner, sheet, path, &context.origin)?,
        seconds,
    })
}

fn text_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    control: &ControlNode,
    extra: &ExtraBindings,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    let ControlNode::Text {
        style,
        label,
        align,
        active_color,
        font,
        weight,
        ..
    } = control
    else {
        unreachable!("text_spec is called only for text")
    };
    Ok(ControlSpec::Text {
        style: *style,
        label: optional_text(context, machine, label.as_deref(), path)?,
        color: intern_optional_color(machine.interner, extra.color.as_ref(), &context.origin)?,
        active_color: *active_color,
        active: optional_binding(context, machine, extra.active.as_ref())?,
        align: *align,
        font: context.optional_param(font.as_ref(), path)?,
        weight: context.optional_param(weight.as_ref(), path)?,
    })
}

fn tree_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    extra: &ExtraBindings,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Tree {
        query: optional_binding(context, machine, extra.query.as_ref())?,
        search: extra.query.is_some() || extra.writes.query.is_some(),
        toggle: extra.writes.toggle.is_some(),
    })
}

fn segmented_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    items: &[String],
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Segmented {
        items: intern_texts(context, interner, items, path, &context.origin)?,
    })
}

fn select_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    label: &str,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Select {
        label: intern_text(context, interner, label, path, &context.origin)?,
    })
}

fn swatch_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    role: ColorRole,
    label: &str,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Swatch {
        role,
        label: intern_text(context, interner, label, path, &context.origin)?,
    })
}

fn caption_spec(
    context: &Context<'_>,
    control: &ControlNode,
    path: &str,
    machine: &mut Expander<'_, '_>,
) -> Result<Option<ControlSpec>, UiDocError> {
    let spec = match control {
        ControlNode::NavItem {
            label, icon, style, ..
        } => ControlSpec::NavItem {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
            icon: context.param(icon, path)?,
            style: *style,
        },
        ControlNode::TabLarge { label, .. } => ControlSpec::TabLarge {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
        },
        ControlNode::Button {
            label,
            icon,
            active_label,
            style,
            frame,
            ..
        } => ControlSpec::Button {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
            icon: context.optional_param(icon.as_ref(), path)?,
            active_label: optional_text(context, machine, active_label.as_deref(), path)?,
            style: *style,
            frame: *frame,
        },
        ControlNode::Bpm { placeholder, .. } => ControlSpec::Bpm {
            placeholder: optional_text(context, machine, placeholder.as_deref(), path)?,
        },
        ControlNode::Fader { style, label, .. } => ControlSpec::Fader {
            style: *style,
            label: optional_text(context, machine, label.as_deref(), path)?,
        },
        ControlNode::Cell {
            label, highlighted, ..
        } => ControlSpec::Cell {
            label: optional_text(context, machine, label.as_deref(), path)?,
            highlighted: *highlighted,
        },
        ControlNode::Readout {
            label,
            tone,
            framed,
            ..
        } => ControlSpec::Readout {
            label: optional_text(context, machine, label.as_deref(), path)?,
            tone: *tone,
            framed: *framed,
        },
        ControlNode::Chip { label, style, .. } => ControlSpec::Chip {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
            style: *style,
        },
        ControlNode::Knob { label, .. } => ControlSpec::Knob {
            label: optional_text(context, machine, label.as_deref(), path)?,
        },
        _ => return Ok(fixed_control_spec(control)),
    };
    Ok(Some(spec))
}

/// The artwork a document names, the one a press shows instead, and the flag
/// that says which of them stands.
fn lottie_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    control: &ControlNode,
    extra: &ExtraBindings,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    let ControlNode::Lottie {
        artwork,
        active_artwork,
        seconds,
        ..
    } = control
    else {
        unreachable!("lottie_spec is called only for an artwork")
    };
    Ok(ControlSpec::Lottie {
        artwork: intern_text(context, machine.interner, artwork, path, &context.origin)?,
        active_artwork: intern_optional_text(
            context,
            machine.interner,
            active_artwork.as_deref(),
            path,
            &context.origin,
        )?,
        active: optional_binding(context, machine, extra.active.as_ref())?,
        seconds: *seconds,
    })
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
        | ControlNode::Object { .. }
        | ControlNode::Optional { .. }
        | ControlNode::Popover { .. }
        | ControlNode::Modal { .. }
        | ControlNode::Reveal { .. }
        | ControlNode::Placed { .. }
        | ControlNode::Pressable { .. }
        | ControlNode::Stage { .. }
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
        | ControlNode::Shader { .. }
        | ControlNode::Custom { .. }
        | ControlNode::Lottie { .. }
        | ControlNode::Sprite { .. }
        | ControlNode::Table { .. }
        | ControlNode::Search { .. }
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
    machine: &mut Expander<'_, '_>,
    label: Option<&str>,
    path: &str,
) -> Result<Option<InternId>, UiDocError> {
    intern_optional_text(context, machine.interner, label, path, &context.origin)
}

fn optional_binding(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    binding: Option<&BindingRef>,
) -> Result<Option<Binding>, UiDocError> {
    intern_optional_binding(machine.interner, binding, &context.origin)
}

pub(in crate::expand) fn intern_optional_color(
    interner: &mut Interner,
    source: Option<&ColorSource>,
    origin: &SourceUri,
) -> Result<Option<ColorSpec>, UiDocError> {
    source
        .map(|source| source.try_map(|read| intern_binding(interner, read, origin)))
        .transpose()
}

fn wave_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    style: WaveStyle,
    badge: Option<&str>,
    zoom: Option<&BindingRef>,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Wave {
        style,
        badge: intern_optional_text(context, machine.interner, badge, path, &context.origin)?,
        zoom: intern_optional_binding(machine.interner, zoom, &context.origin)?,
    })
}

fn table_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    control: &ControlNode,
    extra: &ExtraBindings,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    let ControlNode::Table {
        columns,
        footer,
        padding_left,
        padding_right,
        ..
    } = control
    else {
        unreachable!("table_spec is called only for a table")
    };
    let columns = context
        .optional_param(columns.as_ref(), path)?
        .unwrap_or_default();
    let frame = TableFrame::new(*padding_left, *padding_right, *footer);
    let mut resolved: Vec<TableColumn> = Vec::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        let label = resolve_text_key(
            context.text,
            column.label(),
            &context.origin,
            &format!("{path}/columns/{index}/label"),
        )?;
        let mut resolved_column = TableColumn::new(
            column.id(),
            label,
            column.style(),
            column.width(),
            column.flexible(),
        );
        if let Some(write) = column.write() {
            resolved_column = resolved_column.with_write(context.substitute(write, path)?);
        }
        resolved.push(resolved_column);
    }
    Ok(ControlSpec::Table {
        columns: resolved,
        status: intern_optional_binding(machine.interner, extra.status.as_ref(), &context.origin)?,
        frame,
        columns_state: intern_optional_binding(
            machine.interner,
            extra.columns_state.as_ref(),
            &context.origin,
        )?,
        width: intern_optional_binding(
            machine.interner,
            extra.writes.width.as_ref(),
            &context.origin,
        )?,
    })
}
