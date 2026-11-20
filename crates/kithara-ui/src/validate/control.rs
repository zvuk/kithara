use std::collections::{BTreeMap, BTreeSet};

use super::{
    binding::{BindingSide, binding_parts, check_binding},
    slots::{column_writes, primary, read_slots, write_slots},
};
use crate::{
    error::UiDocError,
    expand::ControlSite,
    ids::{EndpointId, SourceUri},
    module::{BindingRef, ControlNode, TableColumn},
    registry::{EndpointCategory, EndpointRegistry, ValueKind},
};

pub(crate) fn check_controls(
    site: ControlSite<'_>,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
    kinds: &BTreeSet<String>,
) -> Result<(), UiDocError> {
    check_context_scope(site, origin)?;
    check_custom_kind(site, origin, kinds)?;
    if matches!(site.control, ControlNode::Table { .. }) {
        check_table(
            site.columns,
            site.columns_state,
            site.path,
            origin,
            endpoints,
        )?;
    }
    for (binding, kind) in read_slots(site) {
        check_binding(
            binding,
            BindingSide::Read,
            kind,
            site.path,
            origin,
            endpoints,
        )?;
    }
    if let Some(binding) = site.write
        && primary(site.control).is_none()
    {
        check_binding(
            binding,
            BindingSide::Write,
            None,
            site.path,
            origin,
            endpoints,
        )?;
    }
    for slot in write_slots(site) {
        check_binding(
            slot.binding,
            slot.side,
            Some(slot.kind),
            site.path,
            origin,
            endpoints,
        )?;
    }
    for (_, binding, _, kind) in column_writes(site) {
        check_binding(
            &binding,
            BindingSide::Write,
            Some(kind),
            site.path,
            origin,
            endpoints,
        )?;
    }
    Ok(())
}

#[cfg(feature = "render")]
pub(crate) fn check_artworks(
    site: ControlSite<'_>,
    origin: &SourceUri,
    declared: &BTreeMap<String, String>,
) -> Result<(), UiDocError> {
    for name in site.artworks.into_iter().flatten() {
        if !declared.contains_key(name) && crate::lottie::builtin_artwork(name).is_none() {
            return Err(UiDocError::UnknownArtwork {
                origin: origin.clone(),
                name: name.to_owned(),
                path: site.path.to_owned(),
            });
        }
    }
    Ok(())
}

/// Refuses a document that names an extension the application never
/// registered, so the kind a host resolves at mount is one it already has.
fn check_custom_kind(
    site: ControlSite<'_>,
    origin: &SourceUri,
    kinds: &BTreeSet<String>,
) -> Result<(), UiDocError> {
    let ControlNode::Custom { kind, .. } = site.control else {
        return Ok(());
    };
    if kinds.contains(kind) {
        return Ok(());
    }
    Err(UiDocError::UnknownCustomKind {
        origin: origin.clone(),
        path: site.path.to_owned(),
        kind: kind.clone(),
    })
}

pub(crate) fn shader_uniform_kind(
    name: &str,
    binding: &BindingRef,
    path: &str,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<ValueKind, UiDocError> {
    let Some((category, id, with)) = binding_parts(binding) else {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: view_id(binding).to_owned(),
            path: path.to_owned(),
            detail: "view state is not allowed on this side".to_owned(),
        });
    };
    if !matches!(
        category,
        EndpointCategory::Parameter | EndpointCategory::Telemetry | EndpointCategory::Model
    ) {
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
    if !matches!(
        endpoint.value,
        ValueKind::Bool | ValueKind::Scalar | ValueKind::Stereo
    ) {
        return Err(UiDocError::Shader {
            origin: origin.clone(),
            path: path.to_owned(),
            detail: format!(
                "uniform {name:?} binds {kind} endpoint {id:?}; expected Bool, Scalar, or Stereo",
                kind = endpoint.value,
                id = id.0,
            ),
        });
    }
    check_scopes(id, with, endpoint, path, origin)?;
    Ok(endpoint.value)
}

pub(super) fn check_context_scope(
    site: ControlSite<'_>,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    let ControlNode::ContextBar { scope_items, .. } = site.control else {
        return Ok(());
    };
    let enabled = !scope_items.is_empty();
    if enabled == site.scope.is_some() && enabled == site.write.is_some() {
        return Ok(());
    }
    Err(UiDocError::InvalidContextScope {
        origin: origin.clone(),
        path: site.path.to_owned(),
    })
}

pub(super) fn check_table(
    columns: &[TableColumn],
    columns_state: Option<&BindingRef>,
    path: &str,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    let Some(binding) = columns_state else {
        return Ok(());
    };
    let Some((category, id, with)) = binding_parts(binding) else {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: view_id(binding).to_owned(),
            path: path.to_owned(),
            detail: "view state is not allowed on this side".to_owned(),
        });
    };
    if !matches!(
        category,
        EndpointCategory::Parameter | EndpointCategory::Telemetry | EndpointCategory::Model
    ) {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            detail: format!("{category} endpoint is not allowed on this side"),
        });
    }
    for column in columns {
        let derived = EndpointId(format!("{}.{}", id.0, column.id()));
        let Some(endpoint) = endpoints.endpoint(category, &derived) else {
            continue;
        };
        if endpoint.value != ValueKind::Bool {
            return Err(UiDocError::BindingType {
                origin: origin.clone(),
                id: derived.0,
                path: path.to_owned(),
                expected: ValueKind::Bool.to_string(),
                got: endpoint.value.to_string(),
            });
        }
        check_scopes(&derived, with, endpoint, path, origin)?;
    }
    Ok(())
}

pub(super) fn check_scopes(
    id: &EndpointId,
    with: &BTreeMap<String, String>,
    endpoint: &crate::registry::EndpointDesc,
    path: &str,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    for scope in &endpoint.scopes {
        if !with.contains_key(scope) {
            return Err(UiDocError::MissingScope {
                origin: origin.clone(),
                id: id.0.clone(),
                scope: scope.clone(),
                path: path.to_owned(),
            });
        }
    }
    for scope in with.keys() {
        if !endpoint.scopes.contains(scope) {
            return Err(UiDocError::UnknownScope {
                origin: origin.clone(),
                id: id.0.clone(),
                scope: scope.clone(),
                path: path.to_owned(),
            });
        }
    }
    Ok(())
}

/// The name a view binding carries, for the errors raised where one is not
/// allowed at all.
fn view_id(binding: &BindingRef) -> &str {
    match binding {
        BindingRef::View { id, .. } => &id.0,
        _ => "",
    }
}
