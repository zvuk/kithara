use std::collections::BTreeMap;

use super::{SlotWrites, machine::Context};
use crate::{
    error::UiDocError,
    ids::NodeId,
    module::{BindingRef, ColorSource, ControlNode},
    param::Param,
    size::SizeSpec,
};

#[derive(Clone, Copy)]
pub(super) struct ControlFields<'a> {
    pub(super) id: &'a NodeId,
    pub(super) read: Option<&'a BindingRef>,
    pub(super) size: Option<SizeSpec>,
    pub(super) write: Option<&'a BindingRef>,
}

#[derive(Default)]
pub(super) struct ExtraBindings {
    pub(super) active: Option<BindingRef>,
    pub(super) color: Option<ColorSource>,
    pub(super) columns_state: Option<BindingRef>,
    pub(super) query: Option<BindingRef>,
    pub(super) scope: Option<BindingRef>,
    pub(super) status: Option<BindingRef>,
    pub(super) zoom: Option<BindingRef>,
    pub(super) writes: ExtraWrites,
    pub(super) uniforms: Vec<(String, BindingRef)>,
}

/// The owned side of [`SlotWrites`] that a control's own fields fill.
#[derive(Default)]
pub(super) struct ExtraWrites {
    pub(super) loop_end: Option<BindingRef>,
    pub(super) loop_start: Option<BindingRef>,
    pub(super) query: Option<BindingRef>,
    pub(super) toggle: Option<BindingRef>,
    pub(super) width: Option<BindingRef>,
    pub(super) zoom: Option<BindingRef>,
}

#[derive(Clone, Copy)]
pub(super) struct ExtraBindingRefs<'a> {
    pub(super) active: Option<&'a BindingRef>,
    pub(super) columns_state: Option<&'a BindingRef>,
    pub(super) color_read: Option<&'a BindingRef>,
    pub(super) query: Option<&'a BindingRef>,
    pub(super) scope: Option<&'a BindingRef>,
    pub(super) status: Option<&'a BindingRef>,
    pub(super) zoom: Option<&'a BindingRef>,
    pub(super) writes: SlotWrites<'a>,
}

impl ExtraBindings {
    pub(super) fn as_refs(&self) -> ExtraBindingRefs<'_> {
        let writes = &self.writes;
        ExtraBindingRefs {
            active: self.active.as_ref(),
            columns_state: self.columns_state.as_ref(),
            color_read: self.color.as_ref().and_then(ColorSource::binding),
            query: self.query.as_ref(),
            scope: self.scope.as_ref(),
            status: self.status.as_ref(),
            zoom: self.zoom.as_ref(),
            writes: SlotWrites {
                zoom: writes.zoom.as_ref(),
                loop_start: writes.loop_start.as_ref(),
                loop_end: writes.loop_end.as_ref(),
                query: writes.query.as_ref(),
                width: writes.width.as_ref(),
                toggle: writes.toggle.as_ref(),
                ..SlotWrites::default()
            },
        }
    }

    pub(super) fn substitute(
        context: &Context<'_>,
        control: &ControlNode,
        path: &str,
    ) -> Result<Self, UiDocError> {
        let mut extra = Self::default();
        if !extra.substitute_panel(context, control, path)? {
            extra.substitute_display(context, control, path)?;
        }
        Ok(extra)
    }

    fn substitute_display(
        &mut self,
        context: &Context<'_>,
        control: &ControlNode,
        path: &str,
    ) -> Result<(), UiDocError> {
        match control {
            ControlNode::Text { active, color, .. } => {
                self.substitute_text(context, active.as_ref(), color.as_ref(), path)?;
            }
            ControlNode::Glyph { active, color, .. } => {
                self.substitute_glyph(context, active.as_ref(), color.as_ref(), path)?;
            }
            ControlNode::Lottie { active, .. } | ControlNode::StatusDot { active, .. } => {
                self.active = substituted(active.as_ref(), context, path)?;
            }
            ControlNode::Shader { uniforms, .. } => {
                self.uniforms = substitute_uniforms(uniforms, context, path)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn substitute_glyph(
        &mut self,
        context: &Context<'_>,
        active: Option<&BindingRef>,
        color: Option<&Param<ColorSource>>,
        path: &str,
    ) -> Result<(), UiDocError> {
        self.active = substituted(active, context, path)?;
        let resolved = context.optional_param(color, path)?;
        self.color = resolved
            .as_ref()
            .map(|color| substitute_color(color, context, path))
            .transpose()?;
        Ok(())
    }

    fn substitute_panel(
        &mut self,
        context: &Context<'_>,
        control: &ControlNode,
        path: &str,
    ) -> Result<bool, UiDocError> {
        match control {
            ControlNode::Table {
                columns_state,
                status,
                write_width,
                ..
            } => {
                self.columns_state = substituted(columns_state.as_ref(), context, path)?;
                self.status = substituted(status.as_ref(), context, path)?;
                self.writes.width = substituted(write_width.as_ref(), context, path)?;
            }
            ControlNode::Tree {
                query,
                write_query,
                toggle,
                ..
            } => {
                self.query = substituted(query.as_ref(), context, path)?;
                self.writes.query = substituted(write_query.as_ref(), context, path)?;
                self.writes.toggle = substituted(toggle.as_ref(), context, path)?;
            }
            ControlNode::ContextBar { scope, .. } => {
                self.scope = substituted(scope.as_ref(), context, path)?;
            }
            ControlNode::Wave { .. } => return self.substitute_wave(context, control, path),
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn substitute_text(
        &mut self,
        context: &Context<'_>,
        active: Option<&BindingRef>,
        color: Option<&ColorSource>,
        path: &str,
    ) -> Result<(), UiDocError> {
        self.active = substituted(active, context, path)?;
        self.color = color
            .map(|color| substitute_color(color, context, path))
            .transpose()?;
        Ok(())
    }

    fn substitute_wave(
        &mut self,
        context: &Context<'_>,
        control: &ControlNode,
        path: &str,
    ) -> Result<bool, UiDocError> {
        let ControlNode::Wave {
            zoom,
            write_zoom,
            write_loop_start,
            write_loop_end,
            ..
        } = control
        else {
            return Ok(false);
        };
        self.zoom = substituted(zoom.as_ref(), context, path)?;
        self.writes.zoom = substituted(write_zoom.as_ref(), context, path)?;
        self.writes.loop_start = substituted(write_loop_start.as_ref(), context, path)?;
        self.writes.loop_end = substituted(write_loop_end.as_ref(), context, path)?;
        Ok(true)
    }
}

impl<'a> ControlFields<'a> {
    pub(super) const fn new(
        id: &'a NodeId,
        size: Option<SizeSpec>,
        read: Option<&'a BindingRef>,
        write: Option<&'a BindingRef>,
    ) -> Self {
        Self {
            id,
            read,
            size,
            write,
        }
    }
}

fn substituted(
    declared: Option<&BindingRef>,
    context: &Context<'_>,
    path: &str,
) -> Result<Option<BindingRef>, UiDocError> {
    declared
        .map(|binding| context.substitute(binding, path))
        .transpose()
}

pub(in crate::expand) fn substitute_color(
    source: &ColorSource,
    context: &Context<'_>,
    path: &str,
) -> Result<ColorSource, UiDocError> {
    source.try_map(|read| context.substitute(read, path))
}

fn substitute_uniforms(
    uniforms: &BTreeMap<String, BindingRef>,
    context: &Context<'_>,
    path: &str,
) -> Result<Vec<(String, BindingRef)>, UiDocError> {
    uniforms
        .iter()
        .map(|(name, binding)| Ok((name.clone(), context.substitute(binding, path)?)))
        .collect()
}
