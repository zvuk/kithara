use std::collections::BTreeMap;

use serde::de::DeserializeOwned;

use super::{Binding, BindingKind, machine::Context};
use crate::{
    error::UiDocError,
    ids::{EndpointId, InternId, Interner, SourceUri},
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

pub(super) fn intern_module_text(
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

pub(super) fn intern_module_text_opt(
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

pub(crate) fn substitute_binding(
    args: &BTreeMap<String, String>,
    origin: &SourceUri,
    binding: &BindingRef,
    path: &str,
) -> Result<BindingRef, UiDocError> {
    let (BindingRef::Command { id, with }
    | BindingRef::Parameter { id, with }
    | BindingRef::Telemetry { id, with }
    | BindingRef::Model { id, with }) = binding;
    let id = EndpointId(substitute(args, origin, &id.0, path)?);
    let with = substitute_map(args, origin, with, path)?;
    Ok(match binding {
        BindingRef::Command { .. } => BindingRef::Command { id, with },
        BindingRef::Parameter { .. } => BindingRef::Parameter { id, with },
        BindingRef::Telemetry { .. } => BindingRef::Telemetry { id, with },
        BindingRef::Model { .. } => BindingRef::Model { id, with },
    })
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

pub(crate) fn intern_binding(
    interner: &mut Interner,
    binding: &BindingRef,
    origin: &SourceUri,
) -> Result<Binding, UiDocError> {
    let (kind, id, with) = match binding {
        BindingRef::Command { id, with } => (BindingKind::Command, id, with),
        BindingRef::Parameter { id, with } => (BindingKind::Parameter, id, with),
        BindingRef::Telemetry { id, with } => (BindingKind::Telemetry, id, with),
        BindingRef::Model { id, with } => (BindingKind::Model, id, with),
    };
    let BindingParts { id, key, with } = intern_binding_parts(interner, &id.0, with, origin)?;
    Ok(Binding {
        with,
        kind,
        id,
        key,
    })
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
    let substituted = substitute(&context.args, &context.origin, value, path)?;
    let resolved = resolve_text_key(context.text, &substituted, origin, path)?;
    interner.intern(resolved, origin)
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::ids::DocId;

    fn with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn catalog(pairs: &[(&str, &str)]) -> TextDoc {
        TextDoc {
            id: DocId("test".to_owned()),
            schema: "kithara.text".to_owned(),
            version: 1,
            entries: pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    fn origin() -> SourceUri {
        SourceUri("test.ron".into())
    }

    #[kithara::test]
    fn a_leading_at_resolves_a_catalog_key() {
        let text = catalog(&[("menu.modules", "Modules")]);
        assert_eq!(
            resolve_text_key(&text, "@menu.modules", &origin(), "node").unwrap(),
            "Modules"
        );
    }

    #[kithara::test]
    fn doubled_at_escapes_to_a_literal_at() {
        let text = catalog(&[]);
        assert_eq!(
            resolve_text_key(&text, "@@handle", &origin(), "node").unwrap(),
            "@handle"
        );
    }

    #[kithara::test]
    fn a_mid_string_at_is_not_a_marker() {
        let text = catalog(&[]);
        assert_eq!(
            resolve_text_key(&text, "user@example.com", &origin(), "node").unwrap(),
            "user@example.com"
        );
    }

    #[kithara::test]
    fn a_value_without_at_passes_through_unchanged() {
        let text = catalog(&[]);
        assert_eq!(
            resolve_text_key(&text, "PLAY", &origin(), "node").unwrap(),
            "PLAY"
        );
    }

    #[kithara::test]
    fn an_unknown_key_is_an_error_carrying_origin_and_path() {
        let text = catalog(&[]);
        let error = resolve_text_key(&text, "@missing", &origin(), "node/path").unwrap_err();
        assert!(matches!(
            error,
            UiDocError::UnknownTextKey { key, path, .. }
                if key == "missing" && path == "node/path"
        ));
    }

    #[kithara::test]
    fn scoped_key_is_the_bare_id_without_scopes() {
        assert_eq!(
            scoped_key("player.output.volume", &BTreeMap::new()),
            "player.output.volume"
        );
    }

    #[kithara::test]
    fn scoped_key_appends_sorted_scope_pairs() {
        assert_eq!(
            scoped_key("deck.playback.playing", &with(&[("deck", "b")])),
            "deck.playback.playing@deck=b"
        );
        assert_eq!(
            scoped_key(
                "deck.playback.playing",
                &with(&[("layer", "2"), ("deck", "a")])
            ),
            "deck.playback.playing@deck=a,layer=2"
        );
    }
}
