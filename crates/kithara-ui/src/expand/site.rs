use super::machine::Context;
use crate::{
    error::UiDocError,
    ids::NodeId,
    module::{AdaptivePolicy, BindingRef, ControlNode},
    size::SizeSpec,
};

#[derive(Clone, Copy)]
pub(super) struct ControlFields<'a> {
    pub(super) id: &'a NodeId,
    pub(super) size: Option<SizeSpec>,
    pub(super) read: Option<&'a BindingRef>,
    pub(super) write: Option<&'a BindingRef>,
    pub(super) adaptive: &'a AdaptivePolicy,
}

pub(super) struct ExtraBindings {
    pub(super) columns_state: Option<BindingRef>,
    pub(super) query: Option<BindingRef>,
    pub(super) scope: Option<BindingRef>,
    pub(super) zoom: Option<BindingRef>,
    pub(super) active: Option<BindingRef>,
}

#[derive(Clone, Copy)]
pub(super) struct ExtraBindingRefs<'a> {
    pub(super) columns_state: Option<&'a BindingRef>,
    pub(super) query: Option<&'a BindingRef>,
    pub(super) scope: Option<&'a BindingRef>,
    pub(super) zoom: Option<&'a BindingRef>,
    pub(super) active: Option<&'a BindingRef>,
}

impl ExtraBindings {
    pub(super) fn substitute(
        context: &Context<'_>,
        control: &ControlNode,
        path: &str,
    ) -> Result<Self, UiDocError> {
        let columns_state = match control {
            ControlNode::TrackList { columns_state, .. } => {
                substituted(columns_state.as_ref(), context, path)?
            }
            _ => None,
        };
        let query = match control {
            ControlNode::Tree { query, .. } => substituted(query.as_ref(), context, path)?,
            _ => None,
        };
        let scope = match control {
            ControlNode::ContextBar { scope, .. } => substituted(scope.as_ref(), context, path)?,
            _ => None,
        };
        let zoom = match control {
            ControlNode::Wave { zoom, .. } => substituted(zoom.as_ref(), context, path)?,
            _ => None,
        };
        let active = match control {
            ControlNode::Text { active, .. } | ControlNode::Glyph { active, .. } => {
                substituted(active.as_ref(), context, path)?
            }
            _ => None,
        };
        Ok(Self {
            columns_state,
            query,
            scope,
            zoom,
            active,
        })
    }

    pub(super) fn as_refs(&self) -> ExtraBindingRefs<'_> {
        ExtraBindingRefs {
            columns_state: self.columns_state.as_ref(),
            query: self.query.as_ref(),
            scope: self.scope.as_ref(),
            zoom: self.zoom.as_ref(),
            active: self.active.as_ref(),
        }
    }
}

impl<'a> ControlFields<'a> {
    pub(super) fn new(
        id: &'a NodeId,
        size: Option<SizeSpec>,
        read: Option<&'a BindingRef>,
        write: Option<&'a BindingRef>,
        adaptive: &'a AdaptivePolicy,
    ) -> Self {
        Self {
            id,
            size,
            read,
            write,
            adaptive,
        }
    }
}

/// Substitutes an optional binding a control declares beside its read and
/// write, leaving an absent one absent.
fn substituted(
    declared: Option<&BindingRef>,
    context: &Context<'_>,
    path: &str,
) -> Result<Option<BindingRef>, UiDocError> {
    declared
        .map(|binding| context.substitute(binding, path))
        .transpose()
}
