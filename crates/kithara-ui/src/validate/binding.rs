use std::collections::BTreeMap;

use super::control::check_scopes;
use crate::{
    error::UiDocError,
    ids::{EndpointId, SourceUri, StateId},
    module::{BindingRef, ControlNode},
    registry::{EndpointCategory, EndpointRegistry, ValueKind},
};

pub(in crate::validate) mod consts {
    use super::*;

    pub(in crate::validate) const BLOCK_HIDDEN: ValueKind = ValueKind::Bool;
}

#[derive(Clone, Copy)]
pub(super) enum BindingSide {
    Read,
    Write,
    ModelWrite,
}

pub(crate) const fn value_kinds(control: &ControlNode) -> (Option<ValueKind>, Option<ValueKind>) {
    match control {
        ControlNode::Bpm { .. } => (Some(ValueKind::Waveform), None),
        ControlNode::DeckSummary { .. }
        | ControlNode::Text { .. }
        | ControlNode::Readout { .. } => (Some(ValueKind::Text), None),
        ControlNode::ContextBar { .. } => (Some(ValueKind::Text), Some(ValueKind::Scalar)),
        ControlNode::Optional { .. } => (Some(consts::BLOCK_HIDDEN), None),
        ControlNode::Popover { .. } => (Some(ValueKind::Bool), None),
        ControlNode::Pressable { .. } => (None, Some(ValueKind::Trigger)),
        ControlNode::Button { .. }
        | ControlNode::NavItem { .. }
        | ControlNode::TabLarge { .. }
        | ControlNode::Toggle { .. }
        | ControlNode::Checkbox { .. }
        | ControlNode::Chip { .. } => (Some(ValueKind::Bool), Some(ValueKind::Trigger)),
        ControlNode::Adaptive { .. }
        | ControlNode::Time { .. }
        | ControlNode::Scalar { .. }
        | ControlNode::Meter { .. }
        | ControlNode::Sprite { .. }
        | ControlNode::Lottie { .. }
        | ControlNode::Object { .. } => (Some(ValueKind::Scalar), None),
        ControlNode::Crossfader { .. }
        | ControlNode::Fader { .. }
        | ControlNode::Knob { .. }
        | ControlNode::Segmented { .. }
        | ControlNode::Vis { .. } => (Some(ValueKind::Scalar), Some(ValueKind::Scalar)),
        ControlNode::Wave { .. } => (Some(ValueKind::Waveform), Some(ValueKind::Scalar)),
        ControlNode::PortalMap { .. } => (Some(ValueKind::PortalMap), None),
        ControlNode::Range { .. } => (Some(ValueKind::Range), Some(ValueKind::Scalar)),
        ControlNode::Table { .. } => (Some(ValueKind::Table), None),
        ControlNode::Tree { .. } => (Some(ValueKind::Tree), None),
        ControlNode::VuStereo { .. } | ControlNode::VuVertical { .. } => {
            (Some(ValueKind::Stereo), Some(ValueKind::Scalar))
        }
        ControlNode::Row { .. } | ControlNode::Column { .. } => (None, Some(ValueKind::Scalar)),
        ControlNode::Placed { .. } => (Some(ValueKind::Point), Some(ValueKind::Point)),
        ControlNode::Include { .. }
        | ControlNode::Reveal { .. }
        | ControlNode::Scroll { .. }
        | ControlNode::Stage { .. }
        | ControlNode::Slot { .. }
        | ControlNode::Brand { .. }
        | ControlNode::Spacer { .. }
        | ControlNode::Divider { .. }
        | ControlNode::PresetSelector { .. }
        | ControlNode::SettingsButton { .. }
        | ControlNode::WindowDrag { .. }
        | ControlNode::TitleBar { .. }
        | ControlNode::WindowControls { .. }
        | ControlNode::Glyph { .. }
        | ControlNode::Select { .. }
        | ControlNode::StatusDot { .. }
        | ControlNode::Swatch { .. }
        | ControlNode::Cell { .. }
        | ControlNode::Custom { .. }
        | ControlNode::Shader { .. } => (None, None),
    }
}

/// The endpoint one binding addresses: the vocabulary it belongs to, its name,
/// and the scopes the document filled in.
pub(super) type BindingParts<'a> = (
    EndpointCategory,
    &'a EndpointId,
    &'a BTreeMap<String, String>,
);

/// The endpoint one binding addresses, or nothing when it addresses no
/// endpoint at all.
///
/// A [`BindingRef::View`] and a [`BindingRef::Page`] name state the view keeps
/// for itself, which no application declares and no registry can answer for, so
/// they have no parts of this shape.
pub(super) const fn binding_parts(binding: &BindingRef) -> Option<BindingParts<'_>> {
    match binding {
        BindingRef::Command { id, with } => Some((EndpointCategory::Command, id, with)),
        BindingRef::Parameter { id, with } => Some((EndpointCategory::Parameter, id, with)),
        BindingRef::Telemetry { id, with } => Some((EndpointCategory::Telemetry, id, with)),
        BindingRef::Model { id, with } => Some((EndpointCategory::Model, id, with)),
        BindingRef::View { .. } | BindingRef::Page { .. } => None,
    }
}

/// A view binding names no endpoint, so what is checked is the side it sits on:
/// state reads as a bool, whether it is a flag standing on or a page the state
/// stands at, and is written by a press.
fn check_view(
    id: &StateId,
    side: BindingSide,
    expected_kind: Option<ValueKind>,
    path: &str,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    let wrong = |detail: String| UiDocError::BindingDirection {
        detail,
        origin: origin.clone(),
        id: id.0.clone(),
        path: path.to_owned(),
    };
    let wanted = if matches!(side, BindingSide::Read) {
        ValueKind::Bool
    } else {
        ValueKind::Trigger
    };
    match expected_kind {
        Some(kind) if kind == wanted => Ok(()),
        Some(kind) => Err(wrong(format!("view state reads as a bool, not {kind}"))),
        None => Err(wrong("control does not support this side".to_owned())),
    }
}

pub(super) fn check_binding(
    binding: &BindingRef,
    side: BindingSide,
    expected_kind: Option<ValueKind>,
    path: &str,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    if let BindingRef::View { id, .. } | BindingRef::Page { id, .. } = binding {
        return check_view(id, side, expected_kind, path, origin);
    }
    let Some((category, id, with)) = binding_parts(binding) else {
        unreachable!("the view binding is answered above")
    };
    let allowed = match side {
        BindingSide::Read => matches!(
            category,
            EndpointCategory::Parameter | EndpointCategory::Telemetry | EndpointCategory::Model
        ),
        BindingSide::Write => matches!(
            category,
            EndpointCategory::Command | EndpointCategory::Parameter
        ),
        BindingSide::ModelWrite => {
            matches!(
                category,
                EndpointCategory::Command | EndpointCategory::Parameter | EndpointCategory::Model
            )
        }
    };
    if !allowed {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            detail: format!("{category} endpoint is not allowed on this side"),
        });
    }
    let Some(endpoint) = endpoints.endpoint(category, id) else {
        return Err(UiDocError::UnknownEndpoint {
            origin: origin.clone(),
            category: category.to_string(),
            id: id.0.clone(),
            path: path.to_owned(),
        });
    };
    let Some(expected_kind) = expected_kind else {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            detail: "control does not support this side".to_owned(),
        });
    };
    if expected_kind != endpoint.value {
        return Err(UiDocError::BindingType {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            expected: expected_kind.to_string(),
            got: endpoint.value.to_string(),
        });
    }
    check_scopes(id, with, endpoint, path, origin)
}
