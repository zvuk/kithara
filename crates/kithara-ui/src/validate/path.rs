use std::collections::BTreeMap;

use crate::{error::UiDocError, ids::SourceUri};

#[derive(Clone, Debug, Default)]
pub(crate) struct NodePath(Vec<String>);

impl NodePath {
    pub(crate) fn push(&self, segment: impl Into<String>) -> Self {
        let mut next = self.0.clone();
        next.push(segment.into());
        Self(next)
    }

    pub(crate) fn render(&self) -> String {
        if self.0.is_empty() {
            "root".to_owned()
        } else {
            format!("root/{}", self.0.join("/"))
        }
    }
}

pub(crate) fn check_block_path(path: &str, origin: &SourceUri) -> Result<(), UiDocError> {
    let reason = if path.contains('.') {
        "block address must not contain '.'"
    } else if path.contains('@') {
        "block address must not contain '@'"
    } else {
        return Ok(());
    };
    Err(UiDocError::InvalidId {
        origin: origin.clone(),
        id: path.to_owned(),
        reason: reason.to_owned(),
    })
}

pub(super) fn check_block_id(id: &str, origin: &SourceUri) -> Result<(), UiDocError> {
    check_id(id, origin)?;
    check_block_path(id, origin)
}

pub(super) fn check_id(id: &str, origin: &SourceUri) -> Result<(), UiDocError> {
    refuse(
        id,
        origin,
        bad_name(id).map(|reason| format!("id {reason}")),
    )
}

/// A state name reads like a path: a bare name belongs to the module instance
/// that wrote it, and one led by `/` is the screen's own. That mark is scope
/// rather than name, so what has to read as a name is what stands under it.
pub(super) fn check_state_id(id: &str, origin: &SourceUri) -> Result<(), UiDocError> {
    let name = id.strip_prefix('/').unwrap_or(id);
    refuse(
        id,
        origin,
        bad_name(name).map(|reason| format!("state name {reason}")),
    )
}

pub(crate) fn check_fill_key(address: &str, key: &str) -> Result<(), UiDocError> {
    bad_name(key).map_or(Ok(()), |reason| {
        Err(UiDocError::FillKey {
            reason,
            address: address.to_owned(),
            key: key.to_owned(),
        })
    })
}

pub(crate) fn check_scope(
    with: &BTreeMap<String, String>,
    origin: &SourceUri,
    path: &str,
) -> Result<(), UiDocError> {
    with.iter()
        .find_map(|(name, value)| Some((name, value, bad_scope_value(value)?)))
        .map_or(Ok(()), |(name, value, reason)| {
            Err(UiDocError::ScopeValue {
                reason,
                origin: origin.clone(),
                name: name.clone(),
                value: value.clone(),
                path: path.to_owned(),
            })
        })
}

fn bad_scope_value(value: &str) -> Option<&'static str> {
    if value.contains(',') {
        Some("must not contain ','")
    } else if value.contains('=') {
        Some("must not contain '='")
    } else {
        None
    }
}

/// Why this cannot be read as a name, or nothing when it can.
fn bad_name(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("must not be empty")
    } else if name.contains('/') {
        Some("must not contain '/'")
    } else if name.starts_with('$') {
        Some("must not start with '$'")
    } else {
        None
    }
}

fn refuse(id: &str, origin: &SourceUri, reason: Option<String>) -> Result<(), UiDocError> {
    reason.map_or(Ok(()), |reason| {
        Err(UiDocError::InvalidId {
            reason,
            origin: origin.clone(),
            id: id.to_owned(),
        })
    })
}
