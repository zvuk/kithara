use std::collections::BTreeMap;

use serde::de::DeserializeOwned;

use super::super::{Binding, BindingKind, machine::Context};
use crate::{
    error::UiDocError,
    ids::{EndpointId, InternId, Interner, SourceUri, StateId},
    module::BindingRef,
    param::Param,
    text::TextDoc,
};

pub(crate) fn substitute(
    args: &BTreeMap<String, String>,
    origin: &SourceUri,
    value: &str,
    path: &str,
) -> Result<String, UiDocError> {
    if let Some(literal) = value.strip_prefix("$$") {
        return Ok(format!("${literal}"));
    }
    let Some(name) = value.strip_prefix('$') else {
        return Ok(value.to_owned());
    };
    args.get(name)
        .cloned()
        .ok_or_else(|| UiDocError::UnresolvedParam {
            origin: origin.clone(),
            name: name.to_owned(),
            path: path.to_owned(),
        })
}

pub(crate) fn resolve_text_key<'a>(
    text: &'a TextDoc,
    value: &'a str,
    origin: &SourceUri,
    path: &str,
) -> Result<&'a str, UiDocError> {
    if value.starts_with("@@") {
        return Ok(&value[1..]);
    }
    let Some(key) = value.strip_prefix('@') else {
        return Ok(value);
    };
    text.get(key).ok_or_else(|| UiDocError::UnknownTextKey {
        origin: origin.clone(),
        key: key.to_owned(),
        path: path.to_owned(),
    })
}

pub(in crate::expand) fn intern_module_text(
    interner: &mut Interner,
    text: &TextDoc,
    value: &str,
    prefix: &str,
    field: &str,
    origin: &SourceUri,
) -> Result<InternId, UiDocError> {
    let path = format!("{prefix}/{field}");
    let resolved = resolve_text_key(text, value, origin, &path)?;
    interner.intern(resolved, origin)
}

pub(in crate::expand) fn intern_module_text_opt(
    interner: &mut Interner,
    text: &TextDoc,
    value: Option<&str>,
    prefix: &str,
    field: &str,
    origin: &SourceUri,
) -> Result<Option<InternId>, UiDocError> {
    value
        .map(|value| intern_module_text(interner, text, value, prefix, field, origin))
        .transpose()
}

pub(crate) fn resolve_param<T: Clone + DeserializeOwned>(
    args: &BTreeMap<String, String>,
    origin: &SourceUri,
    param: &Param<T>,
    path: &str,
) -> Result<T, UiDocError> {
    let reference = match param {
        Param::Fixed(value) => return Ok(value.clone()),
        Param::Ref(reference) => reference,
    };
    let name = reference
        .strip_prefix('$')
        .ok_or_else(|| UiDocError::BadVariant {
            origin: origin.clone(),
            value: reference.clone(),
            path: path.to_owned(),
        })?;
    let value = substitute(args, origin, reference, path)?;
    ron::from_str::<T>(&value).map_err(|_| UiDocError::BadParamVariant {
        value,
        origin: origin.clone(),
        name: name.to_owned(),
        path: path.to_owned(),
    })
}

pub(crate) fn resolve_optional_param<T: Clone + DeserializeOwned>(
    args: &BTreeMap<String, String>,
    origin: &SourceUri,
    param: Option<&Param<T>>,
    path: &str,
) -> Result<Option<T>, UiDocError> {
    param
        .map(|param| resolve_param(args, origin, param, path))
        .transpose()
}

pub(crate) fn substitute_map(
    args: &BTreeMap<String, String>,
    origin: &SourceUri,
    map: &BTreeMap<String, String>,
    path: &str,
) -> Result<BTreeMap<String, String>, UiDocError> {
    map.iter()
        .map(|(key, value)| Ok((key.clone(), substitute(args, origin, value, path)?)))
        .collect()
}

/// The name one view state answers to, read the way a path is read.
///
/// A bare name is the module instance's own: two includes of one module are
/// two instances at two prefixes, so a state named inside that module is a
/// different state in each without the document saying so, and nodes of one
/// instance share the prefix - which is what lets a popover and the button
/// that closes it name the same state.
///
/// A name led by `/` is the screen's, named from the layout that holds every
/// instance. That is how a nav in one module turns a `Tabs` in another: the
/// two are not one instance, so the state they share can only be the screen's.
pub(crate) fn scoped_state(instance: &str, id: &str) -> String {
    match id.strip_prefix('/') {
        Some(screen) => screen.to_owned(),
        None if instance.is_empty() => id.to_owned(),
        None => format!("{instance}/{id}"),
    }
}

pub(crate) fn substitute_binding(
    args: &BTreeMap<String, String>,
    origin: &SourceUri,
    binding: &BindingRef,
    path: &str,
    instance: &str,
) -> Result<BindingRef, UiDocError> {
    if let BindingRef::View { id, set } = binding {
        let id = substitute(args, origin, &id.0, path)?;
        return Ok(BindingRef::View {
            id: StateId(scoped_state(instance, &id)),
            set: *set,
        });
    }
    if let BindingRef::Page { id, name } = binding {
        let id = substitute(args, origin, &id.0, path)?;
        return Ok(BindingRef::Page {
            id: StateId(scoped_state(instance, &id)),
            name: substitute(args, origin, name, path)?,
        });
    }
    let (BindingRef::Command { id, with }
    | BindingRef::Parameter { id, with }
    | BindingRef::Telemetry { id, with }
    | BindingRef::Model { id, with }) = binding
    else {
        unreachable!("the view and page bindings are answered above")
    };
    let id = EndpointId(substitute(args, origin, &id.0, path)?);
    let with = substitute_map(args, origin, with, path)?;
    Ok(match binding {
        BindingRef::Command { .. } => BindingRef::Command { id, with },
        BindingRef::Parameter { .. } => BindingRef::Parameter { id, with },
        BindingRef::Telemetry { .. } => BindingRef::Telemetry { id, with },
        BindingRef::Model { .. } => BindingRef::Model { id, with },
        BindingRef::View { .. } | BindingRef::Page { .. } => {
            unreachable!("the view and page bindings are answered above")
        }
    })
}

pub(in crate::expand) fn intern_map(
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

/// Canonical scope-qualified endpoint key: `<id>@<k>=<v>[,<k2>=<v2>...]`,
/// scope names in `BTreeMap` order. Hosts key their `Reads` by this form.
#[must_use]
pub fn scoped_key(id: &str, with: &BTreeMap<String, String>) -> String {
    let mut key = String::with_capacity(
        id.len()
            + with
                .iter()
                .map(|(name, value)| name.len() + value.len() + 2)
                .sum::<usize>(),
    );
    key.push_str(id);
    let mut sep = '@';
    for (name, value) in with {
        key.push(sep);
        key.push_str(name);
        key.push('=');
        key.push_str(value);
        sep = ',';
    }
    key
}

struct BindingParts {
    with: BTreeMap<InternId, InternId>,
    id: InternId,
    key: InternId,
}

fn intern_binding_parts(
    interner: &mut Interner,
    id: &str,
    with: &BTreeMap<String, String>,
    origin: &SourceUri,
) -> Result<BindingParts, UiDocError> {
    let id_intern = interner.intern(id, origin)?;
    let key = if with.is_empty() {
        interner.note_binding_key(id);
        id_intern
    } else {
        let scoped = scoped_key(id, with);
        interner.note_binding_key(&scoped);
        interner.intern(&scoped, origin)?
    };
    Ok(BindingParts {
        key,
        id: id_intern,
        with: intern_map(interner, with, origin)?,
    })
}

/// A state with no scope map has no identity beyond the name given under its module instance, so
/// its key is its own id.
pub(crate) fn intern_binding(
    interner: &mut Interner,
    binding: &BindingRef,
    origin: &SourceUri,
) -> Result<Binding, UiDocError> {
    if let BindingRef::View { id, set } = binding {
        let id = interner.intern(&id.0, origin)?;
        return Ok(Binding {
            with: BTreeMap::new(),
            kind: BindingKind::View { set: *set },
            id,
            key: id,
        });
    }
    if let BindingRef::Page { id, name } = binding {
        let name = interner.intern(name, origin)?;
        let id = interner.intern(&id.0, origin)?;
        return Ok(Binding {
            with: BTreeMap::new(),
            kind: BindingKind::Page { name },
            id,
            key: id,
        });
    }
    let (kind, id, with) = match binding {
        BindingRef::Command { id, with } => (BindingKind::Command, id, with),
        BindingRef::Parameter { id, with } => (BindingKind::Parameter, id, with),
        BindingRef::Telemetry { id, with } => (BindingKind::Telemetry, id, with),
        BindingRef::Model { id, with } => (BindingKind::Model, id, with),
        BindingRef::View { .. } | BindingRef::Page { .. } => {
            unreachable!("the view and page bindings are answered above")
        }
    };
    let BindingParts { id, key, with } = intern_binding_parts(interner, &id.0, with, origin)?;
    Ok(Binding {
        with,
        kind,
        id,
        key,
    })
}

pub(in crate::expand) fn intern_optional_binding(
    interner: &mut Interner,
    binding: Option<&BindingRef>,
    origin: &SourceUri,
) -> Result<Option<Binding>, UiDocError> {
    binding
        .map(|binding| intern_binding(interner, binding, origin))
        .transpose()
}

pub(in crate::expand) fn intern_text(
    context: &Context<'_>,
    interner: &mut Interner,
    value: &str,
    path: &str,
    origin: &SourceUri,
) -> Result<InternId, UiDocError> {
    let substituted = substitute(&context.args, &context.origin, value, path)?;
    let resolved = resolve_text_key(context.text, &substituted, origin, path)?;
    interner.intern(resolved, origin)
}

pub(in crate::expand) fn intern_optional_text(
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

pub(in crate::expand) fn intern_texts(
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
