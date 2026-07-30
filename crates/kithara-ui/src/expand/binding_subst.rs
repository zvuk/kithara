use std::collections::BTreeMap;

use super::{Binding, BindingKind, machine::Context};
use crate::{
    error::UiDocError,
    ids::{InternId, Interner, SourceUri},
    module::BindingRef,
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
    let binding = match binding {
        BindingRef::Command { id, with } => BindingRef::Command {
            id: id.clone(),
            with: substitute_map(args, origin, with, path)?,
        },
        BindingRef::Parameter { id, with } => BindingRef::Parameter {
            id: id.clone(),
            with: substitute_map(args, origin, with, path)?,
        },
        BindingRef::Telemetry { id, with } => BindingRef::Telemetry {
            id: id.clone(),
            with: substitute_map(args, origin, with, path)?,
        },
        BindingRef::Model { id, with } => BindingRef::Model {
            id: id.clone(),
            with: substitute_map(args, origin, with, path)?,
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
    id: InternId,
    key: InternId,
    with: BTreeMap<InternId, InternId>,
}

fn intern_binding_parts(
    interner: &mut Interner,
    id: &str,
    with: &BTreeMap<String, String>,
    origin: &SourceUri,
) -> Result<BindingParts, UiDocError> {
    let id_intern = interner.intern(id, origin)?;
    let key = if with.is_empty() {
        id_intern
    } else {
        interner.intern(&scoped_key(id, with), origin)?
    };
    Ok(BindingParts {
        id: id_intern,
        key,
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
        kind,
        id,
        key,
        with,
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
    interner.intern(
        &substitute(&context.args, &context.origin, value, path)?,
        origin,
    )
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

    fn with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
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
