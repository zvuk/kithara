use std::collections::BTreeMap;

use super::{Binding, machine::Context};
use crate::{
    error::UiDocError,
    ids::{InternId, Interner, SourceUri},
    module::BindingRef,
};

pub(super) fn substitute(
    context: &Context<'_>,
    value: &str,
    path: &str,
) -> Result<String, UiDocError> {
    if let Some(literal) = value.strip_prefix("$$") {
        return Ok(format!("${literal}"));
    }
    let Some(name) = value.strip_prefix('$') else {
        return Ok(value.to_owned());
    };
    context
        .args
        .get(name)
        .cloned()
        .ok_or_else(|| UiDocError::UnresolvedParam {
            origin: context.origin.clone(),
            name: name.to_owned(),
            path: path.to_owned(),
        })
}

pub(super) fn substitute_map(
    context: &Context<'_>,
    map: &BTreeMap<String, String>,
    path: &str,
) -> Result<BTreeMap<String, String>, UiDocError> {
    map.iter()
        .map(|(key, value)| Ok((key.clone(), substitute(context, value, path)?)))
        .collect()
}

pub(super) fn substitute_binding(
    context: &Context<'_>,
    binding: &BindingRef,
    path: &str,
) -> Result<BindingRef, UiDocError> {
    let binding = match binding {
        BindingRef::Command { id, with } => BindingRef::Command {
            id: id.clone(),
            with: substitute_map(context, with, path)?,
        },
        BindingRef::Parameter { id, with } => BindingRef::Parameter {
            id: id.clone(),
            with: substitute_map(context, with, path)?,
        },
        BindingRef::Telemetry { id, with } => BindingRef::Telemetry {
            id: id.clone(),
            with: substitute_map(context, with, path)?,
        },
        BindingRef::Model { id, with } => BindingRef::Model {
            id: id.clone(),
            with: substitute_map(context, with, path)?,
        },
    };
    Ok(binding)
}

pub(super) fn intern_map(
    interner: &mut Interner,
    values: &BTreeMap<String, String>,
    origin: &SourceUri,
) -> Result<BTreeMap<InternId, InternId>, UiDocError> {
    values
        .iter()
        .map(|(key, value)| {
            Ok((
                interner.intern(key, origin)?,
                interner.intern(value, origin)?,
            ))
        })
        .collect()
}

pub(super) fn intern_binding(
    interner: &mut Interner,
    binding: &BindingRef,
    origin: &SourceUri,
) -> Result<Binding, UiDocError> {
    match binding {
        BindingRef::Command { id, with } => Ok(Binding::Command {
            id: interner.intern(&id.0, origin)?,
            with: intern_map(interner, with, origin)?,
        }),
        BindingRef::Parameter { id, with } => Ok(Binding::Parameter {
            id: interner.intern(&id.0, origin)?,
            with: intern_map(interner, with, origin)?,
        }),
        BindingRef::Telemetry { id, with } => Ok(Binding::Telemetry {
            id: interner.intern(&id.0, origin)?,
            with: intern_map(interner, with, origin)?,
        }),
        BindingRef::Model { id, with } => Ok(Binding::Model {
            id: interner.intern(&id.0, origin)?,
            with: intern_map(interner, with, origin)?,
        }),
    }
}

pub(super) fn intern_optional_binding(
    interner: &mut Interner,
    binding: Option<&BindingRef>,
    origin: &SourceUri,
) -> Result<Option<Binding>, UiDocError> {
    binding
        .map(|binding| intern_binding(interner, binding, origin))
        .transpose()
}

pub(super) fn intern_text(
    context: &Context<'_>,
    interner: &mut Interner,
    value: &str,
    path: &str,
    origin: &SourceUri,
) -> Result<InternId, UiDocError> {
    interner.intern(&substitute(context, value, path)?, origin)
}

pub(super) fn intern_optional_text(
    context: &Context<'_>,
    interner: &mut Interner,
    value: Option<&str>,
    path: &str,
    origin: &SourceUri,
) -> Result<Option<InternId>, UiDocError> {
    value
        .map(|value| intern_text(context, interner, value, path, origin))
        .transpose()
}

pub(super) fn intern_texts(
    context: &Context<'_>,
    interner: &mut Interner,
    values: &[String],
    path: &str,
    origin: &SourceUri,
) -> Result<Vec<InternId>, UiDocError> {
    values
        .iter()
        .map(|value| intern_text(context, interner, value, path, origin))
        .collect()
}
